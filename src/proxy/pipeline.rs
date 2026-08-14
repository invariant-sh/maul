//! Ordered billable-request pipeline stages.
//!
//! body → metadata → price → admit → transform → fault → execute → meter → report

#![allow(clippy::result_large_err)]

use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::{
    body::{Body, to_bytes},
    extract::Request,
    http::StatusCode,
    http::request::Parts,
    response::Response,
};
use serde_json::Value;

use super::{
    MAX_REQUEST_BODY_BYTES, ProxyState, apply_mutate_after, meter_response, record_billable,
    reverse_proxy,
};
use crate::budget::{BudgetAdmission, CallPermit, MicroUsd, Price};
use crate::fault::Action;
use crate::openai::{ChatRequestMetadata, OpenAiErrorEnvelope};
use crate::proxy::request_transform;
use crate::report::BudgetDecision;
use crate::session;
use crate::usage::UsageOutcome;
use crate::usage::sse::UsageCompletion;

type UsageLatch = Arc<Mutex<Option<(UsageOutcome, Option<MicroUsd>)>>>;

struct ParsedBody {
    parts: Parts,
    value: Value,
    metadata: ChatRequestMetadata,
    session_id: Option<String>,
}

struct PricedRequest {
    parts: Parts,
    value: Value,
    metadata: ChatRequestMetadata,
    price: Option<Price>,
    session_id: Option<String>,
}

struct AdmittedRequest {
    parts: Parts,
    value: Value,
    metadata: ChatRequestMetadata,
    price: Option<Price>,
    permit: CallPermit,
    session_id: Option<String>,
}

struct TransformedRequest {
    metadata: ChatRequestMetadata,
    price: Option<Price>,
    permit: CallPermit,
    session_id: Option<String>,
    request: Request,
}

/// Run the ordered billable pipeline for one already-classified chat completion.
pub async fn handle_billable(
    state: &ProxyState,
    req: Request,
    path: String,
    started: Instant,
) -> Response {
    let parsed = match read_and_parse_body(state, req, &path, started).await {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };
    let priced = match resolve_price(state, parsed, &path, started) {
        Ok(priced) => priced,
        Err(response) => return response,
    };
    let admitted = match admit_call(state, priced, &path, started) {
        Ok(admitted) => admitted,
        Err(response) => return response,
    };
    let transformed = match transform_outbound(state, admitted, &path, started) {
        Ok(transformed) => transformed,
        Err(response) => return response,
    };
    execute_and_meter(
        state,
        transformed.request,
        transformed.metadata,
        transformed.price,
        transformed.permit,
        path,
        started,
        transformed.session_id,
    )
    .await
}

async fn read_and_parse_body(
    state: &ProxyState,
    req: Request,
    path: &str,
    started: Instant,
) -> Result<ParsedBody, Response> {
    let (parts, body) = req.into_parts();
    let header_session = session::from_headers(&parts.headers).map(session::SessionId::into_string);
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
            record_invalid(state, path, started, &response, header_session);
            return Err(response);
        }
    };

    let value = match serde_json::from_slice::<Value>(&body) {
        Ok(value) => value,
        Err(error) => {
            tracing::debug!(%error, "billable request body is not JSON");
            let response = OpenAiErrorEnvelope::new(
                "maul: chat completion request must be valid JSON",
                "invalid_request_error",
                "invalid_json",
            )
            .into_response(StatusCode::BAD_REQUEST);
            record_invalid(state, path, started, &response, header_session);
            return Err(response);
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
            record_invalid(state, path, started, &response, header_session);
            return Err(response);
        }
    };

    let session_id =
        session::from_headers_and_body(&parts.headers, &value).map(session::SessionId::into_string);

    Ok(ParsedBody {
        parts,
        value,
        metadata,
        session_id,
    })
}

fn resolve_price(
    state: &ProxyState,
    parsed: ParsedBody,
    path: &str,
    started: Instant,
) -> Result<PricedRequest, Response> {
    let price = state.pricing.lookup(parsed.metadata.model.as_str());
    if state.budget.snapshot().cost_limit_usd != MicroUsd::ZERO && price.is_none() {
        let response = OpenAiErrorEnvelope::new(
            format!(
                "maul: no price configured for model `{}`",
                parsed.metadata.model.as_str()
            ),
            "invalid_request_error",
            "model_unpriced",
        )
        .into_response(StatusCode::UNPROCESSABLE_ENTITY);
        record_billable(
            state,
            path.to_owned(),
            started,
            response.status().as_u16(),
            None,
            Some(parsed.metadata.model.as_str().to_owned()),
            None,
            BudgetDecision::ModelUnpriced,
            None,
            None,
            parsed.session_id,
        );
        return Err(response);
    }

    Ok(PricedRequest {
        parts: parsed.parts,
        value: parsed.value,
        metadata: parsed.metadata,
        price,
        session_id: parsed.session_id,
    })
}

fn admit_call(
    state: &ProxyState,
    priced: PricedRequest,
    path: &str,
    started: Instant,
) -> Result<AdmittedRequest, Response> {
    let permit = match state.budget.admit() {
        BudgetAdmission::Allowed(permit) => permit,
        BudgetAdmission::CallCapExceeded { .. } => {
            let response = OpenAiErrorEnvelope::new(
                "maul: max_llm_calls budget exceeded",
                "maul_budget_error",
                "max_llm_calls",
            )
            .into_response(StatusCode::TOO_MANY_REQUESTS);
            record_billable(
                state,
                path.to_owned(),
                started,
                response.status().as_u16(),
                Some("budget_exceeded".to_owned()),
                Some(priced.metadata.model.as_str().to_owned()),
                None,
                BudgetDecision::CallCapExceeded,
                None,
                None,
                priced.session_id,
            );
            return Err(response);
        }
        BudgetAdmission::CostCapExceeded { .. } => {
            let response = OpenAiErrorEnvelope::new(
                "maul: max_cost_usd budget exceeded",
                "maul_budget_error",
                "max_cost_usd",
            )
            .into_response(StatusCode::TOO_MANY_REQUESTS);
            record_billable(
                state,
                path.to_owned(),
                started,
                response.status().as_u16(),
                Some("budget_exceeded".to_owned()),
                Some(priced.metadata.model.as_str().to_owned()),
                None,
                BudgetDecision::CostCapExceeded,
                None,
                None,
                priced.session_id,
            );
            return Err(response);
        }
    };
    tracing::debug!(
        call_number = permit.call_number,
        "billable request admitted"
    );

    Ok(AdmittedRequest {
        parts: priced.parts,
        value: priced.value,
        metadata: priced.metadata,
        price: priced.price,
        permit,
        session_id: priced.session_id,
    })
}

fn transform_outbound(
    state: &ProxyState,
    mut admitted: AdmittedRequest,
    path: &str,
    started: Instant,
) -> Result<TransformedRequest, Response> {
    if let Err(error) = request_transform::include_stream_usage(&mut admitted.value) {
        let response = OpenAiErrorEnvelope::new(
            error.to_string(),
            "invalid_request_error",
            "invalid_stream_options",
        )
        .into_response(StatusCode::BAD_REQUEST);
        record_billable(
            state,
            path.to_owned(),
            started,
            response.status().as_u16(),
            None,
            Some(admitted.metadata.model.as_str().to_owned()),
            Some(admitted.permit.call_number),
            BudgetDecision::Allowed,
            None,
            None,
            admitted.session_id,
        );
        return Err(response);
    }

    let body = match serde_json::to_vec(&admitted.value) {
        Ok(body) => body,
        Err(error) => {
            tracing::error!(%error, "failed to serialize transformed request");
            let response = OpenAiErrorEnvelope::new(
                "maul: failed to serialize transformed request",
                "server_error",
                "request_transform_failed",
            )
            .into_response(StatusCode::INTERNAL_SERVER_ERROR);
            record_billable(
                state,
                path.to_owned(),
                started,
                response.status().as_u16(),
                None,
                Some(admitted.metadata.model.as_str().to_owned()),
                Some(admitted.permit.call_number),
                BudgetDecision::Allowed,
                None,
                None,
                admitted.session_id,
            );
            return Err(response);
        }
    };

    Ok(TransformedRequest {
        metadata: admitted.metadata,
        price: admitted.price,
        permit: admitted.permit,
        session_id: admitted.session_id,
        request: Request::from_parts(admitted.parts, Body::from(body)),
    })
}

#[allow(clippy::too_many_arguments)]
async fn execute_and_meter(
    state: &ProxyState,
    req: Request,
    metadata: ChatRequestMetadata,
    price: Option<Price>,
    permit: CallPermit,
    path: String,
    started: Instant,
    session_id: Option<String>,
) -> Response {
    match state.fault.decide() {
        Action::ShortCircuit { scenario, response } => {
            record_billable(
                state,
                path,
                started,
                response.status().as_u16(),
                Some(scenario.to_owned()),
                Some(metadata.model.as_str().to_owned()),
                Some(permit.call_number),
                BudgetDecision::Allowed,
                None,
                None,
                session_id,
            );
            response
        }
        Action::MutateAfter { scenario } => {
            execute_mutate_after(
                state, req, metadata, price, permit, path, started, scenario, session_id,
            )
            .await
        }
        Action::Forward => {
            execute_forward(
                state, req, metadata, price, permit, path, started, session_id,
            )
            .await
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_mutate_after(
    state: &ProxyState,
    req: Request,
    metadata: ChatRequestMetadata,
    price: Option<Price>,
    permit: CallPermit,
    path: String,
    started: Instant,
    scenario: &'static str,
    session_id: Option<String>,
) -> Response {
    let upstream = reverse_proxy(&state.client, state.upstream_base_url.as_str(), req).await;
    // Streaming MutateAfter drains the body; capture tap completion so the
    // report still records pristine upstream usage/cost.
    let (completion, latch) = streaming_usage_latch(metadata.stream);
    let metered = meter_response(
        upstream,
        metadata.stream,
        state.budget.clone(),
        price,
        completion,
    )
    .await;
    let (response, injected) = apply_mutate_after(scenario, metered.response).await;
    let (usage, cost_usd) = take_metered_outcome(latch, metered.usage, metered.cost_usd);
    record_billable(
        state,
        path,
        started,
        response.status().as_u16(),
        injected.map(str::to_owned),
        Some(metadata.model.as_str().to_owned()),
        Some(permit.call_number),
        BudgetDecision::Allowed,
        usage,
        cost_usd,
        session_id,
    );
    response
}

#[allow(clippy::too_many_arguments)]
async fn execute_forward(
    state: &ProxyState,
    req: Request,
    metadata: ChatRequestMetadata,
    price: Option<Price>,
    permit: CallPermit,
    path: String,
    started: Instant,
    session_id: Option<String>,
) -> Response {
    let response = reverse_proxy(&state.client, state.upstream_base_url.as_str(), req).await;
    if metadata.stream {
        let status = response.status().as_u16();
        let report = state.report.clone();
        let report_path = path.clone();
        let report_model = metadata.model.as_str().to_owned();
        let call_number = permit.call_number;
        let started_for_report = started;
        let report_session = session_id.clone();
        let completion: UsageCompletion = Box::new(move |usage, cost_usd| {
            report.record_request_details(
                report_path,
                status,
                started_for_report.elapsed().as_millis() as u64,
                None,
                true,
                Some(report_model),
                Some(call_number),
                BudgetDecision::Allowed,
                Some(usage),
                cost_usd,
                report_session,
            );
        });
        return meter_response(
            response,
            true,
            state.budget.clone(),
            price,
            Some(completion),
        )
        .await
        .response;
    }

    let metered = meter_response(response, false, state.budget.clone(), price, None).await;
    record_billable(
        state,
        path,
        started,
        metered.response.status().as_u16(),
        None,
        Some(metadata.model.as_str().to_owned()),
        Some(permit.call_number),
        BudgetDecision::Allowed,
        metered.usage,
        metered.cost_usd,
        session_id,
    );
    metered.response
}

fn streaming_usage_latch(stream: bool) -> (Option<UsageCompletion>, Option<UsageLatch>) {
    if !stream {
        return (None, None);
    }
    let latch = Arc::new(Mutex::new(None));
    let latch_for_callback = Arc::clone(&latch);
    let completion: UsageCompletion = Box::new(move |usage, cost_usd| {
        *latch_for_callback.lock().expect("usage latch") = Some((usage, cost_usd));
    });
    (Some(completion), Some(latch))
}

fn take_metered_outcome(
    latch: Option<UsageLatch>,
    usage: Option<UsageOutcome>,
    cost_usd: Option<MicroUsd>,
) -> (Option<UsageOutcome>, Option<MicroUsd>) {
    if let Some(latch) = latch {
        return latch
            .lock()
            .expect("usage latch")
            .take()
            .map_or((None, None), |(usage, cost)| (Some(usage), cost));
    }
    (usage, cost_usd)
}

fn record_invalid(
    state: &ProxyState,
    path: &str,
    started: Instant,
    response: &Response,
    session_id: Option<String>,
) {
    record_billable(
        state,
        path.to_owned(),
        started,
        response.status().as_u16(),
        None,
        None,
        None,
        BudgetDecision::InvalidRequest,
        None,
        None,
        session_id,
    );
}
