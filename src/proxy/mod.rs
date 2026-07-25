//! Pass-through reverse proxy: bounded request body + streamed upstream response.

mod headers;
mod upstream;

pub use headers::{HOP_BY_HOP, hop_by_hop_filter, strip_content_length};
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

use crate::fault::{Action, FORCE_500, FaultEngine};
use crate::report::ReportHandle;

/// Hard cap for inbound request bodies (chat JSON is small; SSE lives on the response).
pub const MAX_REQUEST_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Shared handles needed to proxy + inject faults + record metrics.
#[derive(Clone)]
pub struct ProxyState {
    pub client: Client,
    pub upstream_base_url: Arc<String>,
    pub fault: Arc<FaultEngine>,
    pub report: ReportHandle,
}

/// Entry point for each agent request: maybe short-circuit, else forward upstream.
pub async fn handle(state: &ProxyState, req: Request) -> Response {
    let started = Instant::now();
    let path = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str().to_owned())
        .unwrap_or_else(|| "/".to_owned());

    match state.fault.decide() {
        Action::ShortCircuit(response) => {
            let status = response.status().as_u16();
            state.report.record_request(
                path,
                status,
                started.elapsed().as_millis() as u64,
                Some(FORCE_500.to_owned()),
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

/// Forward an inbound request to the configured OpenAI-compatible upstream.
pub async fn reverse_proxy(client: &Client, upstream_base_url: &str, req: Request) -> Response {
    let method = req.method().clone();
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str().to_owned())
        .unwrap_or_else(|| "/".to_owned());
    let url = build_upstream_url(upstream_base_url, &path_and_query);

    // Strip Content-Length: we rebuild the body bytes and let reqwest set framing.
    let headers = strip_content_length(hop_by_hop_filter(req.headers().clone()));
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
    let response_headers =
        strip_content_length(hop_by_hop_filter(upstream_response.headers().clone()));
    let response_body = Body::from_stream(upstream_response.bytes_stream());

    let mut response = Response::new(response_body);
    *response.status_mut() = status;
    *response.headers_mut() = response_headers;
    response
}
