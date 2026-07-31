//! Pass-through reverse proxy: bounded request body + streamed upstream response.

mod headers;
pub mod request_transform;
mod upstream;

pub use headers::{
    HOP_BY_HOP, hop_by_hop_filter, prepare_response_headers, prepare_upstream_request_headers,
    strip_content_length,
};
pub use upstream::build_upstream_url;

use std::sync::Arc;
use std::time::Instant;

use axum::{
    body::{Body, to_bytes},
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use reqwest::Client;

use crate::fault::{Action, FaultEngine, malform_tool_call_json};
use crate::report::ReportHandle;

/// Hard cap for inbound request bodies (chat JSON is small; SSE lives on the response).
pub const MAX_REQUEST_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Cap when buffering a response to apply MutateAfter (non-streaming JSON only for now).
pub const MAX_MUTATE_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Shared handles needed to proxy + inject faults + record metrics.
#[derive(Clone)]
pub struct ProxyState {
    pub client: Client,
    pub upstream_base_url: Arc<String>,
    pub fault: Arc<FaultEngine>,
    pub report: ReportHandle,
}

/// Entry point for each agent request: maybe short-circuit, mutate-after, or forward.
pub async fn handle(state: &ProxyState, req: Request) -> Response {
    let started = Instant::now();
    let path = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str().to_owned())
        .unwrap_or_else(|| "/".to_owned());

    match state.fault.decide() {
        Action::ShortCircuit { scenario, response } => {
            let status = response.status().as_u16();
            state.report.record_request(
                path,
                status,
                started.elapsed().as_millis() as u64,
                Some(scenario.to_owned()),
            );
            response
        }
        Action::MutateAfter { scenario } => {
            let upstream =
                reverse_proxy(&state.client, state.upstream_base_url.as_str(), req).await;
            let (response, injected) = apply_mutate_after(scenario, upstream).await;
            let status = response.status().as_u16();
            state.report.record_request(
                path,
                status,
                started.elapsed().as_millis() as u64,
                injected.map(str::to_owned),
            );
            response
        }
        Action::Forward => {
            let response =
                reverse_proxy(&state.client, state.upstream_base_url.as_str(), req).await;
            let status = response.status().as_u16();
            state
                .report
                .record_request(path, status, started.elapsed().as_millis() as u64, None);
            response
        }
    }
}

/// Buffer the upstream body, corrupt tool_calls JSON when possible, rebuild the response.
pub async fn apply_mutate_after(
    scenario: &'static str,
    response: Response,
) -> (Response, Option<&'static str>) {
    let status = response.status();
    let headers = response.headers().clone();
    let body = match to_bytes(response.into_body(), MAX_MUTATE_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%error, scenario, "mutate-after: response body too large or failed");
            return (
                (
                    StatusCode::BAD_GATEWAY,
                    format!("maul: failed to buffer response for {scenario}"),
                )
                    .into_response(),
                None,
            );
        }
    };

    match malform_tool_call_json(&body) {
        Some(mutated) => {
            tracing::warn!(scenario, "mutate-after: corrupted tool_call arguments");
            let headers = prepare_response_headers(headers);
            let mut out = Response::new(Body::from(mutated));
            *out.status_mut() = status;
            *out.headers_mut() = headers;
            (out, Some(scenario))
        }
        None => {
            let preview = String::from_utf8_lossy(&body);
            let preview = preview.chars().take(80).collect::<String>();
            tracing::warn!(
                scenario,
                body_preview = %preview,
                "mutate-after: body not JSON/SSE; passing through"
            );
            let headers = prepare_response_headers(headers);
            let mut out = Response::new(Body::from(body));
            *out.status_mut() = status;
            *out.headers_mut() = headers;
            (out, None)
        }
    }
}

/// Forward an inbound request to the configured OpenAI-compatible upstream.
pub async fn reverse_proxy(client: &Client, upstream_base_url: &str, req: Request) -> Response {
    let method = req.method().clone();
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str().to_owned())
        .unwrap_or_else(|| "/".to_owned());
    let url = build_upstream_url(upstream_base_url, &path_and_query);

    // Force Accept-Encoding: identity so MutateAfter sees plaintext JSON/SSE.
    let headers = prepare_upstream_request_headers(req.headers().clone());
    let body = req.into_body();

    tracing::debug!(%method, %url, "proxying request");

    let body_bytes = match to_bytes(body, MAX_REQUEST_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%error, "request body exceeds limit or failed to read");
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("maul: request body exceeds {MAX_REQUEST_BODY_BYTES} byte limit"),
            )
                .into_response();
        }
    };

    let upstream_response = match client
        .request(method, &url)
        .headers(headers)
        .body(body_bytes)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            tracing::error!(%error, %url, "upstream request failed");
            return (
                StatusCode::BAD_GATEWAY,
                format!("maul: upstream error: {error}"),
            )
                .into_response();
        }
    };

    let status = StatusCode::from_u16(upstream_response.status().as_u16())
        .unwrap_or(StatusCode::BAD_GATEWAY);
    let response_headers = prepare_response_headers(upstream_response.headers().clone());
    let response_body = Body::from_stream(upstream_response.bytes_stream());

    let mut response = Response::new(response_body);
    *response.status_mut() = status;
    *response.headers_mut() = response_headers;
    response
}
