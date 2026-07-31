//! End-to-end MutateAfter / ShortCircuit via proxy::handle + wiremock.
//!
//! These mirror the real agent demos: gzip Accept-Encoding, SSE streams,
//! tool-call JSON, and report accounting when mutation cannot apply.

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use flate2::Compression;
use flate2::write::GzEncoder;
use maul::budget::MicroUsd;
use maul::budget::{BudgetLimits, BudgetTracker};
use maul::config::{Budget, Config};
use maul::fault::{FORCE_500, FaultEngine, MALFORMED_TOOL_CALL_JSON};
use maul::pricing::PricingRegistry;
use maul::proxy::{ProxyState, apply_mutate_after, handle};
use maul::report::spawn_collector;
use reqwest::Client;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn test_client() -> Client {
    Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build()
        .expect("client")
}

fn state(upstream: &str, scenarios: Vec<&str>, probability: f64) -> ProxyState {
    let config = Config {
        proxy_listen: "127.0.0.1:7777".into(),
        upstream_base_url: upstream.into(),
        scenarios: scenarios.into_iter().map(str::to_owned).collect(),
        probability,
        seed: 42,
        budget: Budget {
            max_llm_calls: 100,
            max_cost_usd: MicroUsd::from_micro_usd(5_000_000),
        },
        model_prices: std::collections::HashMap::new(),
    };
    let (report, _join) =
        spawn_collector(std::env::temp_dir().join("maul_mutate_test_report.json"));
    ProxyState {
        client: test_client(),
        upstream_base_url: Arc::new(upstream.to_owned()),
        fault: Arc::new(FaultEngine::from_config(&config)),
        budget: BudgetTracker::new(BudgetLimits {
            max_llm_calls: config.budget.max_llm_calls,
            max_cost_usd: config.budget.max_cost_usd,
        }),
        pricing: PricingRegistry::with_overrides(&config.model_prices),
        report,
    }
}

fn gzip(plain: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(plain).unwrap();
    encoder.finish().unwrap()
}

#[tokio::test]
async fn mutate_after_corrupts_tool_call_arguments() {
    let upstream = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{
                  "choices": [{
                    "message": {
                      "role": "assistant",
                      "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "add", "arguments": "{\"a\":21,\"b\":21}"}
                      }]
                    }
                  }]
                }"#,
            "application/json",
        ))
        .expect(1)
        .mount(&upstream)
        .await;

    let state = state(&upstream.uri(), vec![MALFORMED_TOOL_CALL_JSON], 1.0);

    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"model":"gpt-4o-mini"}"#))
        .expect("request");

    let response = handle(&state, req).await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let args = value["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .unwrap();
    assert_eq!(args, "{maul:not-json");
    assert!(serde_json::from_str::<serde_json::Value>(args).is_err());
    assert_eq!(state.report.faults_injected(), 1);
}

#[tokio::test]
async fn mutate_after_despite_client_accept_encoding_gzip() {
    // Exact LangGraph/CrewAI edge case: SDKs send Accept-Encoding: gzip, deflate, br.
    // Maul must force identity upstream so MutateAfter sees plaintext.
    let upstream = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"choices":[{"message":{"role":"assistant","tool_calls":[{"id":"c1","type":"function","function":{"name":"add","arguments":"{\"a\":21,\"b\":21}"}}]}}]}"#,
            "application/json",
        ))
        .expect(1)
        .mount(&upstream)
        .await;

    let state = state(&upstream.uri(), vec![MALFORMED_TOOL_CALL_JSON], 1.0);

    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header("accept-encoding", "gzip, deflate, br")
        .body(Body::from(r#"{"model":"gpt-4o-mini","stream":false}"#))
        .expect("request");

    let response = handle(&state, req).await;
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("{maul:not-json"),
        "expected mutation with gzip Accept-Encoding from client, got: {text}"
    );
    assert_eq!(state.report.faults_injected(), 1);

    let received = upstream.received_requests().await.unwrap();
    assert_eq!(
        received[0]
            .headers
            .get("accept-encoding")
            .and_then(|v| v.to_str().ok()),
        Some("identity")
    );
}

#[tokio::test]
async fn mutate_after_corrupts_sse_tool_call_arguments() {
    let upstream = MockServer::start().await;

    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",",
        "\"type\":\"function\",\"function\":{\"name\":\"add\",\"arguments\":\"{\\\"a\\\":21}\"}}]}}]}\n\n",
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

    let state = state(&upstream.uri(), vec![MALFORMED_TOOL_CALL_JSON], 1.0);

    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header("accept-encoding", "gzip, deflate, br")
        .body(Body::from(r#"{"model":"gpt-4o-mini","stream":true}"#))
        .expect("request");

    let response = handle(&state, req).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("{maul:not-json"),
        "expected malformed args in sse, got: {text}"
    );
    assert_eq!(state.report.faults_injected(), 1);
}

#[tokio::test]
async fn mutate_after_sse_text_only_forces_fault_tool_call() {
    let upstream = MockServer::start().await;

    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"42\"}}]}\n\n",
        "data: [DONE]\n\n"
    );

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse),
        )
        .mount(&upstream)
        .await;

    let state = state(&upstream.uri(), vec![MALFORMED_TOOL_CALL_JSON], 1.0);
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .body(Body::from(r#"{"model":"gpt-4o-mini"}"#))
        .unwrap();

    let response = handle(&state, req).await;
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("maul_injected_tool"));
    assert!(text.contains("{maul:not-json"));
    assert_eq!(state.report.faults_injected(), 1);
}

#[tokio::test]
async fn gzip_body_is_not_counted_as_injected() {
    // Regression: if compressed bytes ever reach MutateAfter, do not claim success.
    let plain = br#"{"choices":[{"message":{"role":"assistant","tool_calls":[{"id":"c1","type":"function","function":{"name":"add","arguments":"{\"a\":1}"}}]}}]}"#;
    let compressed = gzip(plain);

    let mut response = Response::new(Body::from(compressed.clone()));
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert("content-encoding", "gzip".parse().unwrap());
    response
        .headers_mut()
        .insert("content-type", "application/json".parse().unwrap());

    let (out, injected) = apply_mutate_after(MALFORMED_TOOL_CALL_JSON, response).await;
    assert!(injected.is_none());
    assert!(out.headers().get("content-encoding").is_none());
    let body = to_bytes(out.into_body(), 1024 * 1024).await.unwrap();
    assert_eq!(body.as_ref(), compressed.as_slice());
}

#[tokio::test]
async fn mutate_after_strips_content_encoding_when_mutating() {
    let plain = br#"{"choices":[{"message":{"role":"assistant","tool_calls":[{"id":"c1","type":"function","function":{"name":"add","arguments":"{\"a\":1}"}}]}}]}"#;
    let mut response = Response::new(Body::from(plain.to_vec()));
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert("content-encoding", "identity".parse().unwrap());
    response
        .headers_mut()
        .insert("content-type", "application/json".parse().unwrap());

    let (out, injected) = apply_mutate_after(MALFORMED_TOOL_CALL_JSON, response).await;
    assert_eq!(injected, Some(MALFORMED_TOOL_CALL_JSON));
    assert!(out.headers().get("content-encoding").is_none());
    assert_eq!(
        out.headers().get("content-type").unwrap(),
        "application/json"
    );
}

#[tokio::test]
async fn forward_does_not_mutate() {
    let upstream = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#,
            "application/json",
        ))
        .expect(1)
        .mount(&upstream)
        .await;

    let state = state(&upstream.uri(), vec![MALFORMED_TOOL_CALL_JSON], 0.0);

    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .body(Body::from(r#"{"model":"gpt-4o-mini"}"#))
        .expect("request");

    let response = handle(&state, req).await;
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    assert!(
        !String::from_utf8_lossy(&body).contains("{maul:not-json"),
        "body was mutated unexpectedly: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(state.report.faults_injected(), 0);
}

#[tokio::test]
async fn force_500_short_circuits_without_upstream() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&upstream)
        .await;

    let state = state(&upstream.uri(), vec![FORCE_500], 1.0);
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .body(Body::from(r#"{"model":"gpt-4o-mini"}"#))
        .unwrap();

    let response = handle(&state, req).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = to_bytes(response.into_body(), 1024).await.unwrap();
    assert!(String::from_utf8_lossy(&body).contains("force_500"));
    assert_eq!(state.report.faults_injected(), 1);
}
