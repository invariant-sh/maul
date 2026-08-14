//! Session identifier validation and extraction priority.

use axum::http::HeaderMap;
use maul::session::{
    MAX_SESSION_ID_LEN, SESSION_HEADER, from_headers, from_headers_and_body, parse_session_id,
};
use serde_json::json;

fn headers_with(value: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(SESSION_HEADER, value.parse().expect("header value"));
    headers
}

#[test]
fn parse_session_id_rejects_empty_whitespace_control_and_oversize() {
    assert!(parse_session_id("").is_none());
    assert!(parse_session_id("   ").is_none());
    assert!(parse_session_id("session a").is_none());
    assert!(parse_session_id("session\nid").is_none());
    assert!(parse_session_id(&"a".repeat(MAX_SESSION_ID_LEN + 1)).is_none());
    assert_eq!(
        parse_session_id("  run-42:retry  ").unwrap().as_str(),
        "run-42:retry"
    );
}

#[test]
fn header_wins_over_body_fields() {
    let headers = headers_with("from-header");
    let body = json!({
        "user": "from-user",
        "metadata": { "maul_session_id": "from-metadata" }
    });
    assert_eq!(
        from_headers_and_body(&headers, &body).unwrap().as_str(),
        "from-header"
    );
}

#[test]
fn invalid_header_does_not_fall_through_to_body() {
    let headers = headers_with("not a valid id");
    let body = json!({ "user": "from-user" });
    assert!(from_headers_and_body(&headers, &body).is_none());
    assert!(from_headers(&headers).is_none());
}

#[test]
fn user_field_is_used_when_header_is_absent() {
    let body = json!({
        "user": "openai-user",
        "metadata": { "maul_session_id": "from-metadata" }
    });
    assert_eq!(
        from_headers_and_body(&HeaderMap::new(), &body)
            .unwrap()
            .as_str(),
        "openai-user"
    );
}

#[test]
fn metadata_is_used_when_header_and_user_are_absent_or_invalid() {
    let body = json!({
        "user": "has spaces",
        "metadata": { "maul_session_id": "from-metadata" }
    });
    assert_eq!(
        from_headers_and_body(&HeaderMap::new(), &body)
            .unwrap()
            .as_str(),
        "from-metadata"
    );
}

#[test]
fn missing_sources_yield_none() {
    assert!(from_headers(&HeaderMap::new()).is_none());
    assert!(from_headers_and_body(&HeaderMap::new(), &json!({"model": "gpt-4o-mini"})).is_none());
}
