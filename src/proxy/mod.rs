//! Pass-through reverse proxy: bounded request body + streamed upstream response.

mod headers;
mod pipeline;
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

use crate::report::ReportHandle;
use crate::{
    budget::{BudgetTracker, MicroUsd, Price},
    fault::{FaultEngine, malform_tool_call_json},
    openai::{OpenAiErrorEnvelope, classify_billable_route},
    pricing::PricingRegistry,
    report::{BudgetDecision, RequestObservation},
    usage::{
        UsageOutcome, UsageUnavailableReason,
        json::extract_usage,
        sse::{SseUsageTap, UsageCompletion},
    },
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

    pipeline::handle_billable(state, req, path, started).await
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn record_billable(
    state: &ProxyState,
    path: String,
    started: Instant,
    status: u16,
    fault: Option<String>,
    model: Option<String>,
    call_number: Option<u64>,
    budget_decision: BudgetDecision,
    usage: Option<UsageOutcome>,
    cost_usd: Option<MicroUsd>,
) {
    state.report.record(RequestObservation::billable(
        path,
        status,
        started.elapsed().as_millis() as u64,
        fault,
        model,
        call_number,
        budget_decision,
        usage,
        cost_usd,
    ));
}

pub(crate) struct MeteredResponse {
    pub response: Response,
    pub usage: Option<UsageOutcome>,
    pub cost_usd: Option<MicroUsd>,
}

pub(crate) async fn meter_response(
    response: Response,
    stream: bool,
    budget: BudgetTracker,
    price: Option<Price>,
    completion: Option<UsageCompletion>,
) -> MeteredResponse {
    let Some(price) = price else {
        if stream {
            return MeteredResponse {
                response: meter_stream_response(response, budget, None, completion),
                usage: None,
                cost_usd: None,
            };
        }
        return MeteredResponse {
            response,
            usage: None,
            cost_usd: None,
        };
    };
    if !response.status().is_success() {
        // Streaming Forward records only via completion — always fire it once.
        if let Some(completion) = completion {
            completion(
                UsageOutcome::Unavailable(UsageUnavailableReason::UpstreamError),
                None,
            );
        }
        return MeteredResponse {
            response,
            usage: Some(UsageOutcome::Unavailable(
                UsageUnavailableReason::UpstreamError,
            )),
            cost_usd: None,
        };
    }
    if stream {
        return MeteredResponse {
            response: meter_stream_response(response, budget, Some(price), completion),
            usage: None,
            cost_usd: None,
        };
    }

    let status = response.status();
    let headers = response.headers().clone();
    let body = match to_bytes(response.into_body(), MAX_MUTATE_BODY_BYTES).await {
        Ok(body) => body,
        Err(error) => {
            tracing::warn!(%error, "usage metering: response body too large or failed");
            return MeteredResponse {
                response: OpenAiErrorEnvelope::new(
                    "maul: response body exceeded metering limit",
                    "server_error",
                    "response_body_too_large",
                )
                .into_response(StatusCode::BAD_GATEWAY),
                usage: Some(UsageOutcome::Unavailable(
                    UsageUnavailableReason::ResponseTooLarge,
                )),
                cost_usd: None,
            };
        }
    };
    let usage = extract_usage(&body);
    let cost_usd = if let UsageOutcome::Metered(usage) = &usage
        && let Ok(cost) = price.calculate(usage)
    {
        budget.commit_cost(cost);
        Some(cost)
    } else {
        None
    };
    let mut output = Response::new(Body::from(body));
    *output.status_mut() = status;
    *output.headers_mut() = prepare_response_headers(headers);
    MeteredResponse {
        response: output,
        usage: Some(usage),
        cost_usd,
    }
}

fn meter_stream_response(
    response: Response,
    budget: BudgetTracker,
    price: Option<Price>,
    completion: Option<UsageCompletion>,
) -> Response {
    let status = response.status();
    let headers = response.headers().clone();
    let stream = response.into_body().into_data_stream();
    let mut output = Response::new(Body::from_stream(SseUsageTap::with_completion(
        stream, budget, price, completion,
    )));
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
