use reqwest::header::{HeaderMap, HeaderName, CONTENT_LENGTH};

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
    !matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailers"
            | "transfer-encoding"
            | "upgrade"
            | "host"
    )
}
