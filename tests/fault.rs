//! Fault engine + body mutator edge cases (real agent / OpenAI shapes).

use maul::budget::MicroUsd;
use maul::config::{Budget, Config};
use maul::fault::{
    Action, FORCE_500, FaultEngine, MALFORMED_TOOL_CALL_JSON, malform_tool_call_json,
};

fn config_with(scenarios: Vec<&str>, probability: f64, seed: u64) -> Config {
    Config {
        proxy_listen: "127.0.0.1:7777".into(),
        upstream_base_url: "https://api.openai.com".into(),
        scenarios: scenarios.into_iter().map(str::to_owned).collect(),
        probability,
        seed,
        budget: Budget {
            max_llm_calls: 100,
            max_cost_usd: MicroUsd::from_micro_usd(5_000_000),
        },
    }
}

fn assert_args_invalid_json(args: &str) {
    assert_eq!(args, "{maul:not-json");
    assert!(
        serde_json::from_str::<serde_json::Value>(args).is_err(),
        "CrewAI/LangGraph parse tool args as JSON; mutated args must fail that parse"
    );
}

#[test]
fn disabled_when_scenario_not_listed() {
    let engine = FaultEngine::from_config(&config_with(vec![], 1.0, 1));
    for _ in 0..20 {
        assert!(matches!(engine.decide(), Action::Forward));
    }
}

#[test]
fn disabled_when_probability_zero() {
    let engine = FaultEngine::from_config(&config_with(vec![FORCE_500], 0.0, 1));
    for _ in 0..20 {
        assert!(matches!(engine.decide(), Action::Forward));
    }
}

#[test]
fn always_injects_force_500_when_probability_one() {
    let engine = FaultEngine::from_config(&config_with(vec![FORCE_500], 1.0, 7));
    for _ in 0..10 {
        match engine.decide() {
            Action::ShortCircuit { scenario, response } => {
                assert_eq!(scenario, FORCE_500);
                assert_eq!(
                    response.status(),
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR
                );
            }
            other => panic!("expected ShortCircuit, got {other:?}"),
        }
    }
}

#[test]
fn always_injects_malformed_tool_call_when_probability_one() {
    let engine = FaultEngine::from_config(&config_with(vec![MALFORMED_TOOL_CALL_JSON], 1.0, 7));
    for _ in 0..10 {
        match engine.decide() {
            Action::MutateAfter { scenario } => {
                assert_eq!(scenario, MALFORMED_TOOL_CALL_JSON);
            }
            other => panic!("expected MutateAfter, got {other:?}"),
        }
    }
}

#[test]
fn same_seed_same_sequence() {
    let a = FaultEngine::from_config(&config_with(vec![FORCE_500], 0.5, 99));
    let b = FaultEngine::from_config(&config_with(vec![FORCE_500], 0.5, 99));

    let seq_a: Vec<bool> = (0..32)
        .map(|_| matches!(a.decide(), Action::ShortCircuit { .. }))
        .collect();
    let seq_b: Vec<bool> = (0..32)
        .map(|_| matches!(b.decide(), Action::ShortCircuit { .. }))
        .collect();

    assert_eq!(seq_a, seq_b);
    assert!(seq_a.iter().any(|injected| *injected));
    assert!(seq_a.iter().any(|injected| !*injected));
}

#[test]
fn unknown_scenario_names_are_ignored() {
    let engine = FaultEngine::from_config(&config_with(vec!["not_a_real_fault"], 1.0, 1));
    for _ in 0..10 {
        assert!(matches!(engine.decide(), Action::Forward));
    }
}

#[test]
fn both_scenarios_enabled_only_emits_known_actions() {
    let engine = FaultEngine::from_config(&config_with(
        vec![FORCE_500, MALFORMED_TOOL_CALL_JSON],
        1.0,
        7,
    ));
    let mut saw_short = false;
    let mut saw_mutate = false;
    for _ in 0..40 {
        match engine.decide() {
            Action::ShortCircuit { scenario, .. } => {
                assert_eq!(scenario, FORCE_500);
                saw_short = true;
            }
            Action::MutateAfter { scenario } => {
                assert_eq!(scenario, MALFORMED_TOOL_CALL_JSON);
                saw_mutate = true;
            }
            Action::Forward => panic!("probability 1.0 must not Forward"),
        }
    }
    assert!(
        saw_short && saw_mutate,
        "seeded RNG should hit both scenarios"
    );
}

#[test]
fn malform_corrupts_existing_tool_call_arguments() {
    let body = br#"{
      "choices": [{
        "message": {
          "role": "assistant",
          "tool_calls": [{
            "id": "call_1",
            "type": "function",
            "function": {"name": "add", "arguments": "{\"a\":1,\"b\":2}"}
          }]
        }
      }]
    }"#;

    let out = malform_tool_call_json(body).expect("should mutate");
    let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let args = value["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .unwrap();
    assert_args_invalid_json(args);
}

#[test]
fn malform_corrupts_every_tool_call_when_model_emits_several() {
    let body = br#"{
      "choices": [{
        "message": {
          "role": "assistant",
          "tool_calls": [
            {"id":"c1","type":"function","function":{"name":"add","arguments":"{\"a\":1}"}},
            {"id":"c2","type":"function","function":{"name":"add","arguments":"{\"a\":2}"}}
          ]
        }
      }]
    }"#;
    let out = malform_tool_call_json(body).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
    for i in 0..2 {
        assert_args_invalid_json(
            value["choices"][0]["message"]["tool_calls"][i]["function"]["arguments"]
                .as_str()
                .unwrap(),
        );
    }
}

#[test]
fn malform_injects_tool_call_when_missing() {
    let body = br#"{
      "choices": [{
        "message": {"role": "assistant", "content": "hello"}
      }]
    }"#;

    let out = malform_tool_call_json(body).expect("should mutate");
    let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_args_invalid_json(
        value["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap(),
    );
}

#[test]
fn malform_empty_choices_injects_tool_call() {
    let out = malform_tool_call_json(br#"{"choices":[]}"#).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_args_invalid_json(
        value["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap(),
    );
}

#[test]
fn malform_choice_without_message_injects() {
    let out = malform_tool_call_json(br#"{"choices":[{"index":0}]}"#).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_args_invalid_json(
        value["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap(),
    );
}

#[test]
fn malform_returns_none_for_non_json() {
    assert!(malform_tool_call_json(b"not-json").is_none());
}

#[test]
fn malform_returns_none_for_gzip_bytes() {
    // Real failure mode: agent Accept-Encoding:gzip → compressed upstream body.
    // Mutator must not pretend it mutated compressed bytes.
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    let plain = br#"{"choices":[{"message":{"role":"assistant","tool_calls":[{"id":"c1","type":"function","function":{"name":"add","arguments":"{\"a\":1}"}}]}}]}"#;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(plain).unwrap();
    let gzipped = encoder.finish().unwrap();
    assert_eq!(gzipped[0..2], [0x1f, 0x8b], "fixture must look like gzip");
    assert!(
        malform_tool_call_json(&gzipped).is_none(),
        "gzip must not parse as JSON/SSE; proxy must force identity instead"
    );
}

#[test]
fn malform_fabricates_completion_when_shape_unknown() {
    let out = malform_tool_call_json(br#"{"ok":true}"#).expect("should fabricate");
    let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert!(value.get("choices").is_some());
    assert_args_invalid_json(
        value["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap(),
    );
}

#[test]
fn malform_corrupts_sse_tool_call_argument_deltas() {
    // OpenAI streams arguments across chunks; each chunk with `arguments` must break.
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",",
        "\"type\":\"function\",\"function\":{\"name\":\"add\",\"arguments\":\"{\\\"a\\\":\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,",
        "\"function\":{\"arguments\":\"21}\"}}]}}]}\n\n",
        "data: [DONE]\n\n"
    );

    let out = malform_tool_call_json(body.as_bytes()).expect("should mutate sse");
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("{maul:not-json"));
    assert!(text.contains("data: [DONE]"));
    assert!(!text.contains("\\\"a\\\":"));
}

#[test]
fn malform_sse_realistic_openai_tool_stream() {
    // Typical stream: role → tool name (no args yet) → arg fragments → finish.
    let body = concat!(
        "data: {\"id\":\"chatcmpl-x\",\"object\":\"chat.completion.chunk\",\"choices\":[{",
        "\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":null},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-x\",\"object\":\"chat.completion.chunk\",\"choices\":[{",
        "\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_abc\",\"type\":\"function\",",
        "\"function\":{\"name\":\"add\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-x\",\"object\":\"chat.completion.chunk\",\"choices\":[{",
        "\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"a\\\":21,\"}}]},",
        "\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-x\",\"object\":\"chat.completion.chunk\",\"choices\":[{",
        "\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"b\\\":21}\"}}]},",
        "\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-x\",\"object\":\"chat.completion.chunk\",\"choices\":[{",
        "\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    );

    let out = malform_tool_call_json(body.as_bytes()).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("{maul:not-json"));
    assert!(text.contains("\"name\":\"add\"") || text.contains("add"));
    assert!(text.contains("data: [DONE]"));
    // Original concatenated JSON fragments must not survive.
    assert!(!text.contains(r#"\"a\":21"#) && !text.contains(r#"{"a":21"#));
}

#[test]
fn malform_sse_appends_done_when_upstream_truncated() {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",",
        "\"type\":\"function\",\"function\":{\"name\":\"add\",\"arguments\":\"{\\\"a\\\":1}\"}}]}}]}\n\n"
    );
    let out = malform_tool_call_json(body.as_bytes()).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("{maul:not-json"));
    assert!(text.contains("data: [DONE]"));
}

#[test]
fn malform_replaces_text_only_sse_with_fault_chunk() {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"hi\"}}]}\n\n",
        "data: [DONE]\n\n"
    );
    let out = malform_tool_call_json(body.as_bytes()).expect("should replace sse");
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("maul_injected_tool"));
    assert!(text.contains("{maul:not-json"));
    assert!(text.contains("data: [DONE]"));
}

#[test]
fn malform_sse_with_leading_whitespace_still_detected() {
    let body = "\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\ndata: [DONE]\n\n";
    let out = malform_tool_call_json(body.as_bytes()).unwrap();
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("maul_injected_tool")
    );
}
