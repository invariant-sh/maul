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
use serde_json::Value;

use crate::report::ReportHandle;
use crate::{
    budget::{BudgetAdmission, BudgetTracker, Price},
    fault::{Action, FaultEngine, malform_tool_call_json},
    openai::{ChatRequestMetadata, OpenAiErrorEnvelope, classify_billable_route},
    pricing::PricingRegistry,
    usage::{UsageOutcome, json::extract_usage, sse::SseUsageTap},
};

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
    pub budget: BudgetTracker,
    pub pricing: PricingRegistry,
    pub report: ReportHandle,
}

/// Entry point for each agent request: classify, admit, fault, execute, and report.
pub async fn handle(state: &ProxyState, req: Request) -> Response {
    let started = Instant::now();
    let path = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str().to_owned())
        .unwrap_or_else(|| "/".to_owned());

    if classify_billable_route(req.method(), req.uri()).is_none() {
        let response = reverse_proxy(&state.client, state.upstream_base_url.as_str(), req).await;
        record_request(state, path, started, response.status().as_u16(), None);
        return response;
    }

    handle_billable(state, req, path, started).await
}

async fn handle_billable(
    state: &ProxyState,
    req: Request,
    path: String,
    started: Instant,
) -> Response {
    let (parts, body) = req.into_parts();
    let body = match to_bytes(body, MAX_REQUEST_BODY_BYTES).await {
        Ok(body) => body,
        Err(error) => {
            tracing::warn!(%error, "request body exceeds limit or failed to read");
            let response = OpenAiErrorEnvelope::new(
                "maul: request body exceeds configured limit",
                "invalid_request_error",
                "request_body_too_large",
            )
            .into_response(StatusCode::PAYLOAD_TOO_LARGE);
            record_request(state, path, started, response.status().as_u16(), None);
            return response;
        }
    };

    let mut value = match serde_json::from_slice::<Value>(&body) {
        Ok(value) => value,
        Err(error) => {
            tracing::debug!(%error, "billable request body is not JSON");
            let response = OpenAiErrorEnvelope::new(
                "maul: chat completion request must be valid JSON",
                "invalid_request_error",
                "invalid_json",
            )
            .into_response(StatusCode::BAD_REQUEST);
            record_request(state, path, started, response.status().as_u16(), None);
            return response;
        }
    };
    let metadata = match ChatRequestMetadata::try_from(&value) {
        Ok(metadata) => metadata,
        Err(error) => {
            let response = OpenAiErrorEnvelope::new(
                error.to_string(),
                "invalid_request_error",
                "invalid_chat_request",
            )
            .into_response(StatusCode::BAD_REQUEST);
            record_request(state, path, started, response.status().as_u16(), None);
            return response;
        }
    };

    let price = state.pricing.lookup(metadata.model.as_str());
    if state.budget.snapshot().cost_limit_usd != crate::budget::MicroUsd::ZERO && price.is_none() {
        let response = OpenAiErrorEnvelope::new(
            format!(
                "maul: no price configured for model `{}`",
                metadata.model.as_str()
            ),
            "invalid_request_error",
            "model_unpriced",
        )
        .into_response(StatusCode::UNPROCESSABLE_ENTITY);
        record_request(state, path, started, response.status().as_u16(), None);
        return response;
    }

    let permit = match state.budget.admit() {
        BudgetAdmission::Allowed(permit) => permit,
        BudgetAdmission::CallCapExceeded { .. } => {
            let response = OpenAiErrorEnvelope::new(
                "maul: max_llm_calls budget exceeded",
                "maul_budget_error",
                "max_llm_calls",
            )
            .into_response(StatusCode::TOO_MANY_REQUESTS);
            record_request(
                state,
                path,
                started,
                response.status().as_u16(),
                Some("budget_exceeded".to_owned()),
            );
            return response;
        }
        BudgetAdmission::CostCapExceeded { .. } => {
            let response = OpenAiErrorEnvelope::new(
                "maul: max_cost_usd budget exceeded",
                "maul_budget_error",
                "max_cost_usd",
            )
            .into_response(StatusCode::TOO_MANY_REQUESTS);
            record_request(
                state,
                path,
                started,
                response.status().as_u16(),
                Some("budget_exceeded".to_owned()),
            );
            return response;
        }
    };
    tracing::debug!(
        call_number = permit.call_number,
        "billable request admitted"
    );

    if let Err(error) = request_transform::include_stream_usage(&mut value) {
        let response = OpenAiErrorEnvelope::new(
            error.to_string(),
            "invalid_request_error",
            "invalid_stream_options",
        )
        .into_response(StatusCode::BAD_REQUEST);
        record_request(state, path, started, response.status().as_u16(), None);
        return response;
    }
    let body = match serde_json::to_vec(&value) {
        Ok(body) => body,
        Err(error) => {
            tracing::error!(%error, "failed to serialize transformed request");
            let response = OpenAiErrorEnvelope::new(
                "maul: failed to serialize transformed request",
                "server_error",
                "request_transform_failed",
            )
            .into_response(StatusCode::INTERNAL_SERVER_ERROR);
            record_request(state, path, started, response.status().as_u16(), None);
            return response;
        }
    };
    let req = Request::from_parts(parts, Body::from(body));

    match state.fault.decide() {
        Action::ShortCircuit { scenario, response } => {
            record_request(
                state,
                path,
                started,
                response.status().as_u16(),
                Some(scenario.to_owned()),
            );
            response
        }
        Action::MutateAfter { scenario } => {
            let upstream =
                reverse_proxy(&state.client, state.upstream_base_url.as_str(), req).await;
            let upstream =
                meter_response(upstream, metadata.stream, state.budget.clone(), price).await;
            let (response, injected) = apply_mutate_after(scenario, upstream).await;
            record_request(
                state,
                path,
                started,
                response.status().as_u16(),
                injected.map(str::to_owned),
            );
            response
        }
        Action::Forward => {
            let response =
                reverse_proxy(&state.client, state.upstream_base_url.as_str(), req).await;
            let response =
                meter_response(response, metadata.stream, state.budget.clone(), price).await;
            record_request(state, path, started, response.status().as_u16(), None);
            response
        }
    }
}

fn record_request(
    state: &ProxyState,
    path: String,
    started: Instant,
    status: u16,
    fault: Option<String>,
) {
    state
        .report
        .record_request(path, status, started.elapsed().as_millis() as u64, fault);
}

async fn meter_response(
    response: Response,
    stream: bool,
    budget: BudgetTracker,
    price: Option<Price>,
) -> Response {
    let Some(price) = price else {
        return response;
    };
    if !response.status().is_success() {
        return response;
    }
    if stream {
        return meter_stream_response(response, budget, price);
    }

    let status = response.status();
    let headers = response.headers().clone();
    let body = match to_bytes(response.into_body(), MAX_MUTATE_BODY_BYTES).await {
        Ok(body) => body,
        Err(error) => {
            tracing::warn!(%error, "usage metering: response body too large or failed");
            return OpenAiErrorEnvelope::new(
                "maul: response body exceeded metering limit",
                "server_error",
                "response_body_too_large",
            )
            .into_response(StatusCode::BAD_GATEWAY);
        }
    };
    if let UsageOutcome::Metered(usage) = extract_usage(&body)
        && let Ok(cost) = price.calculate(&usage)
    {
        budget.commit_cost(cost);
    }
    let mut output = Response::new(Body::from(body));
    *output.status_mut() = status;
    *output.headers_mut() = prepare_response_headers(headers);
    output
}

fn meter_stream_response(response: Response, budget: BudgetTracker, price: Price) -> Response {
    let status = response.status();
    let headers = response.headers().clone();
    let stream = response.into_body().into_data_stream();
    let mut output = Response::new(Body::from_stream(SseUsageTap::new(stream, budget, price)));
    *output.status_mut() = status;
    *output.headers_mut() = headers;
    output
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
