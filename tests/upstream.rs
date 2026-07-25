//! Tests for upstream URL construction.

use maul::proxy::build_upstream_url;

#[test]
fn joins_base_and_path() {
    assert_eq!(
        build_upstream_url("https://api.openai.com", "/v1/chat/completions"),
        "https://api.openai.com/v1/chat/completions"
    );
}

#[test]
fn strips_trailing_slash_on_base() {
    assert_eq!(
        build_upstream_url("https://api.openai.com/", "/v1/models"),
        "https://api.openai.com/v1/models"
    );
}

#[test]
fn strips_multiple_trailing_slashes_on_base() {
    assert_eq!(
        build_upstream_url("https://api.openai.com///", "/v1/models"),
        "https://api.openai.com/v1/models"
    );
}

#[test]
fn empty_path_becomes_root() {
    assert_eq!(
        build_upstream_url("https://api.openai.com", ""),
        "https://api.openai.com/"
    );
}

#[test]
fn preserves_query_string() {
    assert_eq!(
        build_upstream_url("https://api.openai.com", "/v1/chat/completions?foo=1&bar=2"),
        "https://api.openai.com/v1/chat/completions?foo=1&bar=2"
    );
}

#[test]
fn empty_path_with_trailing_slash_base() {
    assert_eq!(
        build_upstream_url("http://localhost:4000/", ""),
        "http://localhost:4000/"
    );
}
