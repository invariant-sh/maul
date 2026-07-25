//! Tests for hop-by-hop header filtering (public API).

use maul::proxy::{HOP_BY_HOP, hop_by_hop_filter, strip_content_length};
use reqwest::header::{
    AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HOST, HeaderMap, HeaderName, HeaderValue,
};

fn map_with(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.insert(
            HeaderName::from_bytes(name.as_bytes()).expect("valid header name"),
            HeaderValue::from_str(value).expect("valid header value"),
        );
    }
    map
}

#[test]
fn keeps_end_to_end_headers() {
    let input = map_with(&[
        ("authorization", "Bearer sk-test"),
        ("content-type", "application/json"),
        ("openai-organization", "org-x"),
    ]);

    let out = hop_by_hop_filter(input);

    assert_eq!(out.get(AUTHORIZATION).unwrap(), "Bearer sk-test");
    assert_eq!(out.get(CONTENT_TYPE).unwrap(), "application/json");
    assert_eq!(out.get("openai-organization").unwrap(), "org-x");
}

#[test]
fn drops_every_hop_by_hop_header() {
    for name in HOP_BY_HOP {
        let input = map_with(&[(name, "value")]);
        let out = hop_by_hop_filter(input);
        assert!(
            out.get(*name).is_none(),
            "expected hop-by-hop header `{name}` to be stripped"
        );
    }
}

#[test]
fn filters_mixed_map_keeping_only_forwardable() {
    let input = map_with(&[
        ("authorization", "Bearer sk-test"),
        ("host", "localhost:7777"),
        ("connection", "close"),
        ("transfer-encoding", "chunked"),
        ("content-type", "application/json"),
        ("keep-alive", "timeout=5"),
    ]);

    let out = hop_by_hop_filter(input);

    assert_eq!(out.len(), 2);
    assert!(out.contains_key(AUTHORIZATION));
    assert!(out.contains_key(CONTENT_TYPE));
    assert!(!out.contains_key(HOST));
    assert!(!out.contains_key("connection"));
    assert!(!out.contains_key("transfer-encoding"));
    assert!(!out.contains_key("keep-alive"));
}

#[test]
fn empty_input_yields_empty_output() {
    assert!(hop_by_hop_filter(HeaderMap::new()).is_empty());
}

#[test]
fn strip_content_length_removes_only_that_header() {
    let input = map_with(&[
        ("content-type", "application/json"),
        ("content-length", "42"),
        ("authorization", "Bearer sk-test"),
    ]);

    let out = strip_content_length(input);

    assert!(out.get(CONTENT_LENGTH).is_none());
    assert_eq!(out.get(CONTENT_TYPE).unwrap(), "application/json");
    assert_eq!(out.get(AUTHORIZATION).unwrap(), "Bearer sk-test");
}

#[test]
fn strip_content_length_is_noop_when_absent() {
    let input = map_with(&[("content-type", "text/event-stream")]);
    let out = strip_content_length(input);
    assert_eq!(out.len(), 1);
    assert_eq!(out.get(CONTENT_TYPE).unwrap(), "text/event-stream");
}
