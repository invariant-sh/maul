//! Integration tests for the pass-through reverse proxy.

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use maul::proxy::reverse_proxy;
use reqwest::Client;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn test_client() -> Client {
    Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build()
        .expect("client")
}

#[tokio::test]
async fn forwards_request_filters_hop_by_hop_and_streams_body() {
    let upstream = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("connection", "keep-alive")
                .set_body_raw(br#"{"ok":true}"#, "application/json"),
        )
        .expect(1)
        .mount(&upstream)
        .await;

    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer sk-test")
        .header("content-type", "application/json")
        .header("host", "localhost:7777")
        .header("connection", "close")
        .body(Body::from(r#"{"model":"gpt-4o-mini"}"#))
        .expect("request");

    let response = reverse_proxy(&test_client(), &upstream.uri(), req).await;
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");

    assert_eq!(
        status,
        StatusCode::OK,
        "body={}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(headers.get("content-type").unwrap(), "application/json");
    assert!(headers.get("connection").is_none());
    assert!(headers.get("transfer-encoding").is_none());
    assert!(headers.get("content-length").is_none());
    assert!(headers.get("host").is_none());
    assert_eq!(&body[..], br#"{"ok":true}"#);

    let received = upstream
        .received_requests()
        .await
        .expect("mock should record requests");
    assert_eq!(received.len(), 1);
    let got = &received[0];
    assert_eq!(got.headers.get("authorization").unwrap(), "Bearer sk-test");
    assert_eq!(got.headers.get("content-type").unwrap(), "application/json");
    if let Some(host) = got.headers.get("host") {
        assert_ne!(host.to_str().unwrap_or_default(), "localhost:7777");
    }
    assert!(got.headers.get("connection").is_none());
    assert_eq!(got.body, br#"{"model":"gpt-4o-mini"}"#.to_vec());
    assert_eq!(
        got.headers
            .get("accept-encoding")
            .map(|v| v.to_str().unwrap()),
        Some("identity"),
        "Maul must force identity so MutateAfter sees plaintext"
    );
}

#[tokio::test]
async fn returns_bad_gateway_when_upstream_is_unreachable() {
    let client = Client::builder()
        .connect_timeout(Duration::from_millis(200))
        .timeout(Duration::from_millis(500))
        .build()
        .expect("client");

    let req = Request::builder()
        .method("GET")
        .uri("/v1/models")
        .body(Body::empty())
        .expect("request");

    let response = reverse_proxy(&client, "http://127.0.0.1:9", req).await;

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("maul: upstream error"),
        "unexpected body: {text}"
    );
}

#[tokio::test]
async fn preserves_upstream_error_status() {
    let upstream = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(401)
                .insert_header("content-type", "application/json")
                .set_body_string(r#"{"error":"unauthorized"}"#),
        )
        .mount(&upstream)
        .await;

    let req = Request::builder()
        .method("GET")
        .uri("/v1/models")
        .body(Body::empty())
        .expect("request");

    let response = reverse_proxy(&test_client(), &upstream.uri(), req).await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    assert_eq!(&body[..], br#"{"error":"unauthorized"}"#);
}

#[tokio::test]
async fn overrides_client_gzip_accept_encoding_with_identity() {
    let upstream = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(br#"{"ok":true}"#, "application/json"),
        )
        .expect(1)
        .mount(&upstream)
        .await;

    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("accept-encoding", "gzip, deflate, br")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .expect("request");

    let _ = reverse_proxy(&test_client(), &upstream.uri(), req).await;

    let got = &upstream.received_requests().await.unwrap()[0];
    assert_eq!(
        got.headers
            .get("accept-encoding")
            .and_then(|v| v.to_str().ok()),
        Some("identity")
    );
}

#[tokio::test]
async fn strips_content_encoding_from_proxied_response_headers() {
    let upstream = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                // Use identity (not gzip+plaintext) so reqwest does not try to inflate.
                .insert_header("content-encoding", "identity")
                .set_body_raw(br#"{"data":[]}"#, "application/json"),
        )
        .mount(&upstream)
        .await;

    let req = Request::builder()
        .method("GET")
        .uri("/v1/models")
        .body(Body::empty())
        .unwrap();

    let response = reverse_proxy(&test_client(), &upstream.uri(), req).await;
    assert!(
        response.headers().get("content-encoding").is_none(),
        "Maul must strip Content-Encoding after rebuilding/streaming the body"
    );
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "application/json"
    );
}

#[tokio::test]
async fn rejects_oversized_request_body() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&upstream)
        .await;

    let oversized = vec![b'x'; maul::proxy::MAX_REQUEST_BODY_BYTES + 1];
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .body(Body::from(oversized))
        .expect("request");

    let response = reverse_proxy(&test_client(), &upstream.uri(), req).await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}
