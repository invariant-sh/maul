//! Integration tests for the ordered billable-request pipeline.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use maul::budget::{BudgetLimits, BudgetTracker, MicroUsd};
use maul::config::{Budget, Config};
use maul::fault::{FORCE_429, FORCE_500, FaultEngine, MALFORMED_TOOL_CALL_JSON};
use maul::pricing::PricingRegistry;
use maul::proxy::{ProxyState, handle};
use maul::report::{BudgetDecision, ReliabilityReport, spawn_collector};
use maul::usage::{UsageOutcome, UsageUnavailableReason};
use reqwest::Client;
use tempfile::{TempDir, tempdir};
use tokio::task::JoinHandle;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct PipelineFixture {
    state: ProxyState,
    _tempdir: TempDir,
    report_path: std::path::PathBuf,
    collector: JoinHandle<()>,
}

fn state(
    upstream: &str,
    scenarios: Vec<&str>,
    max_llm_calls: u64,
    max_cost_usd: u64,
) -> ProxyState {
    fixture(upstream, scenarios, max_llm_calls, max_cost_usd).state
}

fn fixture(
    upstream: &str,
    scenarios: Vec<&str>,
    max_llm_calls: u64,
    max_cost_usd: u64,
) -> PipelineFixture {
    fixture_with(upstream, scenarios, max_llm_calls, max_cost_usd, 1.0, 42)
}

fn fixture_with(
    upstream: &str,
    scenarios: Vec<&str>,
    max_llm_calls: u64,
    max_cost_usd: u64,
    probability: f64,
    seed: u64,
) -> PipelineFixture {
    let config = Config {
        proxy_listen: "127.0.0.1:7777".into(),
        upstream_base_url: upstream.into(),
        scenarios: scenarios.into_iter().map(str::to_owned).collect(),
        probability,
        seed,
        budget: Budget {
            max_llm_calls,
            max_cost_usd: MicroUsd::from_micro_usd(max_cost_usd),
        },
        model_prices: std::collections::HashMap::new(),
    };
    let tempdir = tempdir().expect("tempdir");
    let report_path = tempdir.path().join("reliability_report.json");
    let (report, collector) = spawn_collector(report_path.clone());
    let state = ProxyState {
        client: Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(5))
            .build()
            .expect("test client"),
        upstream_base_url: Arc::new(upstream.to_owned()),
        fault: Arc::new(FaultEngine::from_config(&config)),
        budget: BudgetTracker::new(BudgetLimits {
            max_llm_calls,
            max_cost_usd: MicroUsd::from_micro_usd(max_cost_usd),
        }),
        pricing: PricingRegistry::with_overrides(&config.model_prices),
        report,
    };
    PipelineFixture {
        state,
        _tempdir: tempdir,
        report_path,
        collector,
    }
}

async fn flush_report(fixture: PipelineFixture) -> ReliabilityReport {
    let snapshot = fixture.state.budget.snapshot();
    fixture
        .state
        .report
        .request_shutdown_with_metadata(snapshot, "test");
    tokio::time::timeout(Duration::from_secs(2), fixture.collector)
        .await
        .expect("collector finished")
        .expect("collector join");
    let raw = std::fs::read_to_string(&fixture.report_path).expect("report file");
    serde_json::from_str(&raw).expect("report json")
}

fn completion_request(stream: bool) -> Request<Body> {
    completion_request_for_model("gpt-4o-mini", stream)
}

fn completion_request_for_model(model: &str, stream: bool) -> Request<Body> {
    completion_request_with(model, stream, None, None)
}

fn completion_request_with(
    model: &str,
    stream: bool,
    session_header: Option<&str>,
    body_extra: Option<&str>,
) -> Request<Body> {
    let extra = body_extra.unwrap_or("");
    let mut builder = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json");
    if let Some(session) = session_header {
        builder = builder.header("x-maul-session-id", session);
    }
    builder
        .body(Body::from(format!(
            r#"{{"model":"{model}","stream":{stream}{extra}}}"#
        )))
        .expect("request")
}

#[tokio::test]
async fn call_cap_blocks_after_exactly_one_admitted_request() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(br#"{"choices":[]}"#, "application/json"),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    let state = state(&upstream.uri(), vec![], 1, 0);

    let first = handle(&state, completion_request(false)).await;
    assert_eq!(first.status(), StatusCode::OK);
    let _ = to_bytes(first.into_body(), 1024 * 1024).await.unwrap();

    let second = handle(&state, completion_request(false)).await;
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = to_bytes(second.into_body(), 1024 * 1024).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["error"]["code"], "max_llm_calls");
    assert_eq!(state.budget.snapshot().calls_reserved, 1);
}

#[tokio::test]
async fn concurrent_requests_never_exceed_call_cap() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(br#"{"choices":[]}"#, "application/json"),
        )
        .expect(5)
        .mount(&upstream)
        .await;
    let state = Arc::new(state(&upstream.uri(), vec![], 5, 0));

    let handles = (0..25)
        .map(|_| {
            let state = Arc::clone(&state);
            tokio::spawn(async move { handle(&state, completion_request(false)).await })
        })
        .collect::<Vec<_>>();
    let responses = futures_util::future::join_all(handles)
        .await
        .into_iter()
        .map(|result| result.expect("request task"))
        .collect::<Vec<_>>();

    let allowed = responses
        .iter()
        .filter(|response| response.status() == StatusCode::OK)
        .count();
    let rejected = responses
        .iter()
        .filter(|response| response.status() == StatusCode::TOO_MANY_REQUESTS)
        .count();
    assert_eq!(allowed, 5);
    assert_eq!(rejected, 20);
    assert_eq!(state.budget.snapshot().calls_reserved, 5);
}

#[tokio::test]
async fn observed_cost_blocks_the_next_request() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"choices":[],"usage":{"prompt_tokens":4,"completion_tokens":0,"total_tokens":4}}"#,
            "application/json",
        ))
        .expect(1)
        .mount(&upstream)
        .await;
    let state = state(&upstream.uri(), vec![], 10, 1);

    let first = handle(&state, completion_request(false)).await;
    assert_eq!(first.status(), StatusCode::OK);
    let _ = to_bytes(first.into_body(), 1024 * 1024).await.unwrap();
    assert_eq!(
        state.budget.snapshot().observed_cost_usd,
        MicroUsd::from_micro_usd(1)
    );

    let second = handle(&state, completion_request(false)).await;
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = to_bytes(second.into_body(), 1024 * 1024).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["error"]["code"], "max_cost_usd");
}

#[tokio::test]
async fn missing_usage_is_not_counted_as_zero_cost() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(br#"{"choices":[]}"#, "application/json"),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    let state = state(&upstream.uri(), vec![], 10, 1);

    let response = handle(&state, completion_request(false)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let _ = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    assert_eq!(state.budget.snapshot().observed_cost_usd, MicroUsd::ZERO);
}

#[tokio::test]
async fn completed_sse_usage_is_metered_and_request_transform_is_preserved() {
    let upstream = MockServer::start().await;
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}],\"usage\":null}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":0,\"total_tokens\":4}}\n\n",
        "data: [DONE]\n\n"
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    let state = state(&upstream.uri(), vec![], 10, 0);

    let response = handle(&state, completion_request(true)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    assert!(String::from_utf8_lossy(&body).contains("data: [DONE]"));
    assert_eq!(
        state.budget.snapshot().observed_cost_usd,
        MicroUsd::from_micro_usd(1)
    );

    let received = upstream.received_requests().await.unwrap();
    let request: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(request["stream_options"]["include_usage"], true);
}

#[tokio::test]
async fn non_billable_routes_bypass_faults_and_budget() {
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"data":[]}"#))
        .expect(1)
        .mount(&upstream)
        .await;
    let state = state(&upstream.uri(), vec!["force_500"], 1, 1);

    let request = Request::builder()
        .method("GET")
        .uri("/v1/models")
        .body(Body::empty())
        .unwrap();
    let response = handle(&state, request).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(state.budget.snapshot().calls_reserved, 0);
}

#[tokio::test]
async fn force_429_consumes_call_without_contacting_upstream() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&upstream)
        .await;
    let state = state(&upstream.uri(), vec![FORCE_429], 1, 0);

    let response = handle(&state, completion_request(false)).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers().get("retry-after").unwrap(), "1");
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["error"]["code"], FORCE_429);
    assert_eq!(state.budget.snapshot().calls_reserved, 1);
}

#[tokio::test]
async fn unpriced_model_is_rejected_before_admission() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&upstream)
        .await;
    let fixture = fixture(&upstream.uri(), vec![], 10, 1_000);

    let response = handle(
        &fixture.state,
        completion_request_for_model("totally-unknown-model", false),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["error"]["code"], "model_unpriced");
    assert_eq!(fixture.state.budget.snapshot().calls_reserved, 0);

    let report = flush_report(fixture).await;
    assert_eq!(report.requests.len(), 1);
    assert_eq!(
        report.requests[0].budget_decision,
        BudgetDecision::ModelUnpriced
    );
    assert!(report.requests[0].call_number.is_none());
}

#[tokio::test]
async fn streaming_upstream_error_still_emits_one_report_event() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(502).set_body_raw(
            br#"{"error":{"message":"upstream boom","type":"server_error"}}"#,
            "application/json",
        ))
        .expect(1)
        .mount(&upstream)
        .await;
    let fixture = fixture(&upstream.uri(), vec![], 10, 0);

    let response = handle(&fixture.state, completion_request(true)).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let _ = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    assert_eq!(fixture.state.budget.snapshot().calls_reserved, 1);

    let report = flush_report(fixture).await;
    assert_eq!(report.requests.len(), 1);
    assert_eq!(report.requests[0].call_number, Some(1));
    assert_eq!(
        report.requests[0].usage,
        Some(UsageOutcome::Unavailable(
            UsageUnavailableReason::UpstreamError
        ))
    );
    assert!(report.budget_snapshot.is_some());
    assert_eq!(report.budget_snapshot.unwrap().calls_reserved, 1);
}

#[tokio::test]
async fn mutate_after_streaming_records_pristine_usage_and_cost() {
    let upstream = MockServer::start().await;
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}],\"usage\":null}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":0,\"total_tokens\":4}}\n\n",
        "data: [DONE]\n\n"
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    let fixture = fixture(&upstream.uri(), vec![MALFORMED_TOOL_CALL_JSON], 10, 0);

    let response = handle(&fixture.state, completion_request(true)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let _ = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    assert_eq!(
        fixture.state.budget.snapshot().observed_cost_usd,
        MicroUsd::from_micro_usd(1)
    );

    let report = flush_report(fixture).await;
    assert_eq!(report.requests.len(), 1);
    assert!(matches!(
        report.requests[0].usage,
        Some(UsageOutcome::Metered(_))
    ));
    assert_eq!(
        report.requests[0].cost_usd,
        Some(MicroUsd::from_micro_usd(1))
    );
    assert_eq!(
        report.requests[0].fault_injected.as_deref(),
        Some(MALFORMED_TOOL_CALL_JSON)
    );
}

fn seed_for_fault_pattern(pattern: &[bool]) -> u64 {
    for seed in 0..50_000 {
        let config = Config {
            proxy_listen: "127.0.0.1:7777".into(),
            upstream_base_url: "http://127.0.0.1:1".into(),
            scenarios: vec![FORCE_500.to_owned()],
            probability: 0.5,
            seed,
            budget: Budget {
                max_llm_calls: 100,
                max_cost_usd: MicroUsd::ZERO,
            },
            model_prices: std::collections::HashMap::new(),
        };
        let engine = FaultEngine::from_config(&config);
        let matches_pattern = pattern.iter().all(|&want_fault| {
            matches!(engine.decide(), maul::fault::Action::ShortCircuit { .. }) == want_fault
        });
        if matches_pattern {
            return seed;
        }
    }
    panic!("no fault-engine seed produced pattern {pattern:?}");
}

#[tokio::test]
async fn interleaved_sessions_only_recover_the_session_with_later_2xx() {
    let seed = seed_for_fault_pattern(&[true, false, false]);
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(br#"{"choices":[]}"#, "application/json"),
        )
        .expect(2)
        .mount(&upstream)
        .await;
    let fixture = fixture_with(&upstream.uri(), vec![FORCE_500], 10, 0, 0.5, seed);

    let first = handle(
        &fixture.state,
        completion_request_with("gpt-4o-mini", false, Some("session-a"), None),
    )
    .await;
    assert_eq!(first.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let second = handle(
        &fixture.state,
        completion_request_with("gpt-4o-mini", false, Some("session-b"), None),
    )
    .await;
    assert_eq!(second.status(), StatusCode::OK);
    let _ = to_bytes(second.into_body(), 1024 * 1024).await.unwrap();

    let third = handle(
        &fixture.state,
        completion_request_with("gpt-4o-mini", false, Some("session-a"), None),
    )
    .await;
    assert_eq!(third.status(), StatusCode::OK);
    let _ = to_bytes(third.into_body(), 1024 * 1024).await.unwrap();

    let report = flush_report(fixture).await;
    assert_eq!(report.schema_version, "0.2");
    assert_eq!(report.summary.recovery_events, 1);
    assert_eq!(report.summary.recovered_sessions, 1);
    assert_eq!(report.summary.unrecovered_sessions, 0);
    let session_a = report
        .sessions
        .iter()
        .find(|session| session.session_id == "session-a")
        .expect("session a");
    let session_b = report
        .sessions
        .iter()
        .find(|session| session.session_id == "session-b")
        .expect("session b");
    assert_eq!(session_a.recovered, Some(true));
    assert_eq!(session_b.recovered, None);
}

#[tokio::test]
async fn mutate_after_repeated_2xx_is_not_a_false_recovery() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(br#"{"choices":[]}"#, "application/json"),
        )
        .expect(2)
        .mount(&upstream)
        .await;
    let fixture = fixture(&upstream.uri(), vec![MALFORMED_TOOL_CALL_JSON], 10, 0);

    for _ in 0..2 {
        let response = handle(
            &fixture.state,
            completion_request_with("gpt-4o-mini", false, Some("session-a"), None),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let _ = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    }

    let report = flush_report(fixture).await;
    assert_eq!(report.summary.fault_events, 2);
    assert_eq!(report.summary.recovery_events, 0);
    assert_eq!(report.summary.post_fault_successes, 0);
    assert_eq!(report.summary.unrecovered_sessions, 1);
}

#[tokio::test]
async fn missing_and_invalid_session_ids_are_unattributed() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(br#"{"choices":[]}"#, "application/json"),
        )
        .expect(2)
        .mount(&upstream)
        .await;
    let fixture = fixture_with(&upstream.uri(), vec![], 10, 0, 0.0, 1);

    let missing = handle(&fixture.state, completion_request(false)).await;
    assert_eq!(missing.status(), StatusCode::OK);
    let _ = to_bytes(missing.into_body(), 1024 * 1024).await.unwrap();

    let invalid = handle(
        &fixture.state,
        completion_request_with("gpt-4o-mini", false, Some("not a valid id"), None),
    )
    .await;
    assert_eq!(invalid.status(), StatusCode::OK);
    let _ = to_bytes(invalid.into_body(), 1024 * 1024).await.unwrap();

    let report = flush_report(fixture).await;
    assert!(
        report
            .requests
            .iter()
            .all(|record| record.session_id.is_none())
    );
    assert_eq!(report.summary.unattributed_requests, 2);
    assert_eq!(report.summary.sessions_observed, 0);
}

#[tokio::test]
async fn sse_pass_through_records_session_and_strips_internal_header() {
    let upstream = MockServer::start().await;
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}],\"usage\":null}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":0,\"total_tokens\":4}}\n\n",
        "data: [DONE]\n\n"
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    let fixture = fixture_with(&upstream.uri(), vec![], 10, 0, 0.0, 1);

    let response = handle(
        &fixture.state,
        completion_request_with("gpt-4o-mini", true, Some("sse-session"), None),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    assert!(String::from_utf8_lossy(&body).contains("data: [DONE]"));

    let received = upstream.received_requests().await.unwrap();
    assert!(received[0].headers.get("x-maul-session-id").is_none());

    let report = flush_report(fixture).await;
    assert_eq!(report.requests.len(), 1);
    assert_eq!(
        report.requests[0].session_id.as_deref(),
        Some("sse-session")
    );
    assert_eq!(report.schema_version, "0.2");
}

#[tokio::test]
async fn body_user_field_attributes_session_when_header_is_absent() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(br#"{"choices":[]}"#, "application/json"),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    let fixture = fixture_with(&upstream.uri(), vec![], 10, 0, 0.0, 1);

    let response = handle(
        &fixture.state,
        completion_request_with("gpt-4o-mini", false, None, Some(r#","user":"body-user""#)),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let _ = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();

    let report = flush_report(fixture).await;
    assert_eq!(report.requests[0].session_id.as_deref(), Some("body-user"));
}
