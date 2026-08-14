use reqwest::header::{
    ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, HeaderMap, HeaderName, HeaderValue,
};

use crate::session::SESSION_HEADER;

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

/// Headers for the upstream request: filter hop-by-hop and force plaintext bodies.
///
/// Agents often send `Accept-Encoding: gzip`. If we forward that, OpenAI returns
/// compressed bytes and MutateAfter cannot parse JSON/SSE. Maul is a test proxy,
/// so we prefer identity encoding over saving bandwidth.
pub fn prepare_upstream_request_headers(headers: HeaderMap) -> HeaderMap {
    let mut out = strip_content_length(hop_by_hop_filter(headers));
    out.remove(ACCEPT_ENCODING);
    out.insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
    // Maul-internal correlation only; never forward to the provider.
    if let Ok(name) = HeaderName::from_bytes(SESSION_HEADER.as_bytes()) {
        out.remove(name);
    }
    out
}

/// Headers for a response whose body we (re)built in memory.
pub fn prepare_response_headers(headers: HeaderMap) -> HeaderMap {
    let mut out = strip_content_length(hop_by_hop_filter(headers));
    out.remove(CONTENT_ENCODING);
    out
}

fn should_forward(name: &HeaderName) -> bool {
    !HOP_BY_HOP.contains(&name.as_str())
}
