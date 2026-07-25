//! Response-body mutators for MutateAfter faults.

use serde_json::{Map, Value, json};

/// Invalid JSON fragment written into `function.arguments`.
/// Intentionally fails `serde_json` / agent-side tool-arg parsing.
const MALFORMED_ARGUMENTS: &str = "{maul:not-json";

/// Corrupt OpenAI-compatible chat `tool_calls` arguments into invalid JSON.
///
/// Supports:
/// - non-streaming `application/json` chat.completion bodies
/// - SSE (`text/event-stream`) chat.completion.chunk bodies
///
/// Returns `None` only when the body is neither JSON nor SSE.
pub fn malform_tool_call_json(body: &[u8]) -> Option<Vec<u8>> {
    if let Ok(mut value) = serde_json::from_slice::<Value>(body) {
        if !corrupt_completion(&mut value) {
            return None;
        }
        return serde_json::to_vec(&value).ok();
    }

    if looks_like_sse(body) {
        return Some(malform_sse(body));
    }

    None
}

fn looks_like_sse(body: &[u8]) -> bool {
    // OpenAI streams are `data: {...}\n\n` (sometimes with a leading BOM/whitespace).
    let text = String::from_utf8_lossy(body);
    text.lines()
        .any(|line| line.trim_start().starts_with("data:"))
}

// --- Shared argument corruption ------------------------------------------------

/// Rewrite `function.arguments` when present. Returns whether anything changed.
fn corrupt_arguments(function: &mut Map<String, Value>) -> bool {
    if !function.contains_key("arguments") {
        return false;
    }
    function.insert(
        "arguments".into(),
        Value::String(MALFORMED_ARGUMENTS.into()),
    );
    true
}

fn corrupt_tool_calls_array(tool_calls: &mut [Value]) -> bool {
    let mut any = false;
    for call in tool_calls.iter_mut() {
        if let Some(function) = call.get_mut("function").and_then(Value::as_object_mut)
            && corrupt_arguments(function)
        {
            any = true;
        }
    }
    any
}

/// Corrupt `tool_calls` on a message-like or delta-like object.
fn corrupt_tool_calls_on_message_like(message: &mut Value) -> bool {
    match message.get_mut("tool_calls").and_then(Value::as_array_mut) {
        Some(tool_calls) if !tool_calls.is_empty() => corrupt_tool_calls_array(tool_calls),
        _ => false,
    }
}

// --- Non-streaming chat.completion --------------------------------------------

fn corrupt_completion(value: &mut Value) -> bool {
    let Some(choices) = value.get_mut("choices").and_then(Value::as_array_mut) else {
        *value = synthetic_malformed_completion();
        return true;
    };

    if choices.is_empty() {
        choices.push(json!({
            "index": 0,
            "message": synthetic_malformed_message(),
            "finish_reason": "tool_calls"
        }));
        return true;
    }

    let mut any = false;
    for choice in choices.iter_mut() {
        any |= corrupt_choice(choice);
    }
    any
}

fn corrupt_choice(choice: &mut Value) -> bool {
    let Some(message) = choice.get_mut("message") else {
        return insert_synthetic_message(choice);
    };

    match message.get_mut("tool_calls").and_then(Value::as_array_mut) {
        Some(tool_calls) if !tool_calls.is_empty() => corrupt_tool_calls_array(tool_calls),
        _ => inject_malformed_tool_call(message),
    }
}

fn insert_synthetic_message(choice: &mut Value) -> bool {
    let Some(obj) = choice.as_object_mut() else {
        return false;
    };
    obj.insert("message".into(), synthetic_malformed_message());
    true
}

fn inject_malformed_tool_call(message: &mut Value) -> bool {
    let Some(obj) = message.as_object_mut() else {
        return false;
    };
    obj.insert(
        "tool_calls".into(),
        json!([synthetic_malformed_tool_call()]),
    );
    obj.insert("content".into(), Value::Null);
    true
}

// --- SSE chat.completion.chunk ------------------------------------------------

enum SseEvent {
    Done,
    Json(Value),
    Raw(String),
    Other(String),
}

fn malform_sse(body: &[u8]) -> Vec<u8> {
    let events = parse_sse_events(body);
    let mut mutated_existing = false;
    let mut rendered: Vec<SseEvent> = Vec::with_capacity(events.len());

    for event in events {
        match event {
            SseEvent::Json(mut value) => {
                if corrupt_sse_chunk(&mut value) {
                    mutated_existing = true;
                }
                rendered.push(SseEvent::Json(value));
            }
            other => rendered.push(other),
        }
    }

    if mutated_existing {
        ensure_sse_done(&mut rendered);
        render_sse_events(&rendered)
    } else {
        // Model returned text-only SSE (or empty) — still force the fault.
        synthetic_malformed_sse()
    }
}

fn parse_sse_events(body: &[u8]) -> Vec<SseEvent> {
    let text = String::from_utf8_lossy(body);
    let mut events = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim_start();
        let Some(payload) = trimmed.strip_prefix("data:") else {
            if !trimmed.is_empty() {
                events.push(SseEvent::Other(line.to_owned()));
            }
            continue;
        };

        let payload = payload.trim();
        if payload.is_empty() {
            continue;
        }
        if payload == "[DONE]" {
            events.push(SseEvent::Done);
            continue;
        }

        match serde_json::from_str::<Value>(payload) {
            Ok(value) => events.push(SseEvent::Json(value)),
            Err(_) => events.push(SseEvent::Raw(payload.to_owned())),
        }
    }

    events
}

fn corrupt_sse_chunk(value: &mut Value) -> bool {
    let Some(choices) = value.get_mut("choices").and_then(Value::as_array_mut) else {
        return false;
    };

    let mut any = false;
    for choice in choices.iter_mut() {
        if let Some(delta) = choice.get_mut("delta") {
            any |= corrupt_tool_calls_on_message_like(delta);
        }
        if let Some(message) = choice.get_mut("message") {
            any |= corrupt_tool_calls_on_message_like(message);
        }
    }
    any
}

fn ensure_sse_done(events: &mut Vec<SseEvent>) {
    let has_done = events.iter().any(|e| matches!(e, SseEvent::Done));
    if !has_done {
        events.push(SseEvent::Done);
    }
}

fn render_sse_events(events: &[SseEvent]) -> Vec<u8> {
    let mut out = String::new();
    for event in events {
        match event {
            SseEvent::Done => out.push_str("data: [DONE]\n\n"),
            SseEvent::Json(value) => {
                out.push_str("data: ");
                out.push_str(&value.to_string());
                out.push_str("\n\n");
            }
            SseEvent::Raw(payload) => {
                out.push_str("data: ");
                out.push_str(payload);
                out.push_str("\n\n");
            }
            SseEvent::Other(line) => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out.into_bytes()
}

// --- Synthetics ---------------------------------------------------------------

fn synthetic_malformed_completion() -> Value {
    json!({
        "id": "chatcmpl-maul-fault",
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": synthetic_malformed_message(),
            "finish_reason": "tool_calls"
        }]
    })
}

fn synthetic_malformed_message() -> Value {
    json!({
        "role": "assistant",
        "content": null,
        "tool_calls": [synthetic_malformed_tool_call()]
    })
}

fn synthetic_malformed_tool_call() -> Value {
    json!({
        "id": "call_maul_fault",
        "type": "function",
        "function": {
            "name": "maul_injected_tool",
            "arguments": MALFORMED_ARGUMENTS
        }
    })
}

fn synthetic_malformed_sse() -> Vec<u8> {
    let chunk = json!({
        "id": "chatcmpl-maul-fault",
        "object": "chat.completion.chunk",
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "index": 0,
                    "id": "call_maul_fault",
                    "type": "function",
                    "function": {
                        "name": "maul_injected_tool",
                        "arguments": MALFORMED_ARGUMENTS
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    });
    format!("data: {chunk}\n\ndata: [DONE]\n\n").into_bytes()
}
