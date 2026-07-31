//! Integration tests for the ordered billable-request pipeline.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use maul::budget::{BudgetLimits, BudgetTracker, MicroUsd};
use maul::config::{Budget, Config};
use maul::fault::FaultEngine;
use maul::pricing::PricingRegistry;
use maul::proxy::{ProxyState, handle};
use maul::report::spawn_collector;
use reqwest::Client;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn state(
    upstream: &str,
    scenarios: Vec<&str>,
    max_llm_calls: u64,
    max_cost_usd: u64,
) -> ProxyState {
    let config = Config {
        proxy_listen: "127.0.0.1:7777".into(),
        upstream_base_url: upstream.into(),
        scenarios: scenarios.into_iter().map(str::to_owned).collect(),
        probability: 1.0,
        seed: 42,
        budget: Budget {
            max_llm_calls,
            max_cost_usd: MicroUsd::from_micro_usd(max_cost_usd),
        },
        model_prices: std::collections::HashMap::new(),
    };
    let (report, _collector) =
        spawn_collector(std::env::temp_dir().join("maul_pipeline_test_report.json"));
    ProxyState {
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
    }
}

fn completion_request(stream: bool) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"model":"gpt-4o-mini","stream":{stream}}}"#
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
