use reqwest::header::{CONTENT_LENGTH, HeaderMap, HeaderName};

/// RFC 7230 hop-by-hop / connection-local names (lowercase, as `HeaderName::as_str`).
pub const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
    "host",
];

/// Drop hop-by-hop / connection-local headers; keep everything else to forward.
pub fn hop_by_hop_filter(headers: HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (key, value) in headers.iter() {
        if should_forward(key) {
            out.insert(key.clone(), value.clone());
        }
    }
    out
}

/// After re-streaming a body, drop Content-Length so framing stays consistent.
pub fn strip_content_length(mut headers: HeaderMap) -> HeaderMap {
    headers.remove(CONTENT_LENGTH);
    headers
}

fn should_forward(name: &HeaderName) -> bool {
    !HOP_BY_HOP.contains(&name.as_str())
}
