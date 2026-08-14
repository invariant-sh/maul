//! Bounded session attribution for Maul request records.
//!
//! Extraction priority:
//! 1. `X-Maul-Session-Id` header
//! 2. OpenAI-compatible JSON `user`
//! 3. JSON `metadata.maul_session_id`
//! 4. `None` (unattributed)
//!
//! Invalid dedicated headers do not fall through to body fields. Invalid `user`
//! or metadata values skip to the next body source. Identifiers are never
//! logged here; callers must not log prompt content or credentials.

use axum::http::HeaderMap;
use serde_json::Value;

/// HTTP header agents use to attribute retries to one workflow session.
pub const SESSION_HEADER: &str = "x-maul-session-id";

/// Maximum accepted session identifier length in bytes.
pub const MAX_SESSION_ID_LEN: usize = 128;

/// Validated session identifier used on request records.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SessionId(String);

impl SessionId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

/// Parse a candidate identifier. Empty, oversized, whitespace, and control
/// characters are rejected so reports cannot be poisoned by prompt-sized values.
pub fn parse_session_id(raw: &str) -> Option<SessionId> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_SESSION_ID_LEN {
        return None;
    }
    if trimmed
        .chars()
        .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return None;
    }
    Some(SessionId(trimmed.to_owned()))
}

/// Extract a session id from headers only (non-billable or pre-parse paths).
///
/// A present but invalid `X-Maul-Session-Id` yields `None` and does not
/// consult other sources.
pub fn from_headers(headers: &HeaderMap) -> Option<SessionId> {
    if !headers.contains_key(SESSION_HEADER) {
        return None;
    }
    let raw = headers.get(SESSION_HEADER)?.to_str().ok()?;
    parse_session_id(raw)
}

/// Extract using the documented source priority.
pub fn from_headers_and_body(headers: &HeaderMap, body: &Value) -> Option<SessionId> {
    if headers.contains_key(SESSION_HEADER) {
        return from_headers(headers);
    }
    from_body(body)
}

fn from_body(body: &Value) -> Option<SessionId> {
    if let Some(user) = body.get("user").and_then(Value::as_str)
        && let Some(session) = parse_session_id(user)
    {
        return Some(session);
    }
    body.get("metadata")
        .and_then(|metadata| metadata.get("maul_session_id"))
        .and_then(Value::as_str)
        .and_then(parse_session_id)
}
