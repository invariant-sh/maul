use bytes::Bytes;
use futures_util::{StreamExt, stream};
use maul::budget::{BudgetLimits, BudgetTracker, MicroUsd, Price};
use maul::openai::TokenUsage;
use maul::proxy::request_transform::include_stream_usage;
use maul::usage::{
    UsageOutcome, UsageUnavailableReason,
    json::extract_usage,
    sse::{DEFAULT_MAX_EVENT_BYTES, SseUsageParser, SseUsageTap, UsageCompletion},
};
use serde_json::json;
use std::sync::{Arc, Mutex};

#[test]
fn extracts_usage_from_json_completion() {
    let body = br#"{
      "choices": [],
      "usage": {
        "prompt_tokens": 10,
        "completion_tokens": 4,
        "total_tokens": 14
      }
    }"#;

    assert_eq!(
        extract_usage(body),
        UsageOutcome::Metered(TokenUsage {
            prompt_tokens: 10,
            completion_tokens: 4,
            total_tokens: 14,
        })
    );
}

#[test]
fn rejects_missing_and_inconsistent_json_usage() {
    assert_eq!(
        extract_usage(br#"{"choices":[]}"#),
        UsageOutcome::Unavailable(UsageUnavailableReason::MissingUsage)
    );
    assert_eq!(
        extract_usage(br#"{"usage":{"prompt_tokens":10,"completion_tokens":4,"total_tokens":99}}"#),
        UsageOutcome::Unavailable(UsageUnavailableReason::MalformedUsage)
    );
}

#[test]
fn request_transform_is_noop_for_non_streaming_requests() {
    let mut body = json!({"model":"gpt-4o-mini","stream":false});
    assert!(!include_stream_usage(&mut body).unwrap());
    assert_eq!(body, json!({"model":"gpt-4o-mini","stream":false}));
}

#[test]
fn request_transform_adds_usage_without_removing_options() {
    let mut body = json!({
        "model": "gpt-4o-mini",
        "stream": true,
        "stream_options": {"include_obfuscation": true}
    });
    assert!(include_stream_usage(&mut body).unwrap());
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(body["stream_options"]["include_obfuscation"], true);
    assert!(!include_stream_usage(&mut body).unwrap());
}

#[test]
fn request_transform_rejects_non_object_stream_options() {
    let mut body = json!({"stream":true,"stream_options":[]});
    assert!(include_stream_usage(&mut body).is_err());
}

#[test]
fn sse_parser_handles_arbitrary_chunk_boundaries() {
    let event = concat!(
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,",
        "\"completion_tokens\":4,\"total_tokens\":14}}\n\n",
        "data: [DONE]\n\n"
    );
    let mut parser = SseUsageParser::default();
    for chunk in event.as_bytes().chunks(3) {
        parser.push(chunk);
    }

    assert_eq!(
        parser.finish(),
        UsageOutcome::Metered(TokenUsage {
            prompt_tokens: 10,
            completion_tokens: 4,
            total_tokens: 14,
        })
    );
}

#[test]
fn sse_parser_reports_missing_usage() {
    let mut parser = SseUsageParser::default();
    parser.push(b"data: {\"choices\":[]}\n\ndata: [DONE]\n\n");
    assert_eq!(
        parser.finish(),
        UsageOutcome::Unavailable(UsageUnavailableReason::MissingUsage)
    );
}

#[test]
fn sse_parser_reports_malformed_events_and_oversized_lines() {
    let mut malformed = SseUsageParser::default();
    malformed.push(b"data: not-json\n\n");
    assert_eq!(
        malformed.finish(),
        UsageOutcome::Unavailable(UsageUnavailableReason::MalformedSse)
    );

    let mut oversized = SseUsageParser::new(DEFAULT_MAX_EVENT_BYTES);
    oversized.push(&vec![b'x'; DEFAULT_MAX_EVENT_BYTES + 1]);
    assert_eq!(
        oversized.finish(),
        UsageOutcome::Unavailable(UsageUnavailableReason::MalformedSse)
    );
}

#[tokio::test]
async fn sse_tap_reports_interrupted_streams() {
    let observed = Arc::new(Mutex::new(None));
    let callback_observed = Arc::clone(&observed);
    let completion: UsageCompletion = Box::new(move |outcome, _cost| {
        *callback_observed.lock().expect("callback lock") = Some(outcome);
    });
    let budget = BudgetTracker::new(BudgetLimits {
        max_llm_calls: 1,
        max_cost_usd: MicroUsd::ZERO,
    });
    let stream = stream::iter(vec![
        Ok::<Bytes, &'static str>(Bytes::from_static(b"data: {\"usage\":null}\n\n")),
        Err("upstream disconnected"),
    ]);
    let mut tap = SseUsageTap::with_completion(
        stream,
        budget,
        Some(Price::new(
            MicroUsd::from_micro_usd(1),
            MicroUsd::from_micro_usd(1),
        )),
        Some(completion),
    );

    while tap.next().await.is_some() {}

    assert_eq!(
        observed.lock().expect("test lock").as_ref(),
        Some(&UsageOutcome::Unavailable(
            UsageUnavailableReason::StreamInterrupted
        ))
    );
}

#[tokio::test]
async fn sse_tap_drop_flushes_interrupted_completion() {
    let observed = Arc::new(Mutex::new(None));
    let callback_observed = Arc::clone(&observed);
    let completion: UsageCompletion = Box::new(move |outcome, _cost| {
        *callback_observed.lock().expect("callback lock") = Some(outcome);
    });
    let budget = BudgetTracker::new(BudgetLimits {
        max_llm_calls: 1,
        max_cost_usd: MicroUsd::ZERO,
    });
    let stream = stream::pending::<Result<Bytes, &'static str>>();
    let tap = SseUsageTap::with_completion(
        stream,
        budget,
        Some(Price::new(
            MicroUsd::from_micro_usd(1),
            MicroUsd::from_micro_usd(1),
        )),
        Some(completion),
    );

    drop(tap);

    assert_eq!(
        observed.lock().expect("test lock").as_ref(),
        Some(&UsageOutcome::Unavailable(
            UsageUnavailableReason::StreamInterrupted
        ))
    );
}
