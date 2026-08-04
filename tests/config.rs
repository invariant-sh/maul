//! Tests for YAML config loading.

use maul::budget::MicroUsd;
use maul::config::{Budget, Config, load};
use std::io::Write;
use tempfile::NamedTempFile;

fn write_yaml(contents: &str) -> NamedTempFile {
    let mut file = NamedTempFile::new().expect("temp file");
    file.write_all(contents.as_bytes()).expect("write yaml");
    file
}

#[test]
fn load_parses_complete_config() {
    let file = write_yaml(
        r#"
proxy_listen: "127.0.0.1:7777"
upstream_base_url: "https://api.openai.com"
scenarios: [force_500, malformed_tool_call_json]
probability: 0.25
seed: 99
budget:
  max_llm_calls: 10
  max_cost_usd: 1.5
"#,
    );

    let config = load(file.path().to_str().unwrap()).expect("load ok");

    assert_eq!(
        config,
        Config {
            proxy_listen: "127.0.0.1:7777".into(),
            upstream_base_url: "https://api.openai.com".into(),
            scenarios: vec!["force_500".into(), "malformed_tool_call_json".into()],
            probability: 0.25,
            seed: 99,
            budget: Budget {
                max_llm_calls: 10,
                max_cost_usd: MicroUsd::from_micro_usd(1_500_000),
            },
            model_prices: std::collections::HashMap::new(),
        }
    );
}

#[test]
fn load_parses_empty_scenarios() {
    let file = write_yaml(
        r#"
proxy_listen: "0.0.0.0:7777"
upstream_base_url: "https://api.openai.com"
scenarios: []
probability: 0.0
seed: 0
budget:
  max_llm_calls: 100
  max_cost_usd: 5.0
"#,
    );

    let config = load(file.path().to_str().unwrap()).expect("load ok");
    assert!(config.scenarios.is_empty());
    assert_eq!(config.probability, 0.0);
    assert_eq!(config.seed, 0);
}

#[test]
fn load_errors_when_file_missing() {
    let err = load("/tmp/maul-does-not-exist-please.yaml").unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("No such file")
            || message.contains("not found")
            || message.contains("os error"),
        "unexpected error: {message}"
    );
}

#[test]
fn load_errors_on_invalid_yaml() {
    let file = write_yaml("this: is: broken: yaml: [");
    let err = load(file.path().to_str().unwrap()).unwrap_err();
    assert!(!err.to_string().is_empty());
}

#[test]
fn load_errors_on_missing_required_field() {
    let file = write_yaml(
        r#"
proxy_listen: "0.0.0.0:7777"
upstream_base_url: "https://api.openai.com"
# scenarios missing
probability: 0.0
seed: 0
budget:
  max_llm_calls: 100
  max_cost_usd: 5.0
"#,
    );

    let err = load(file.path().to_str().unwrap()).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("scenarios") || message.contains("missing field"),
        "unexpected error: {message}"
    );
}

#[test]
fn load_errors_on_wrong_field_type() {
    let file = write_yaml(
        r#"
proxy_listen: "0.0.0.0:7777"
upstream_base_url: "https://api.openai.com"
scenarios: []
probability: "not-a-number"
seed: 0
budget:
  max_llm_calls: 100
  max_cost_usd: 5.0
"#,
    );

    assert!(load(file.path().to_str().unwrap()).is_err());
}

#[test]
fn load_rejects_unknown_scenario() {
    let file = write_yaml(
        r#"
proxy_listen: "0.0.0.0:7777"
upstream_base_url: "https://api.openai.com"
scenarios: [not_a_real_fault]
probability: 0.0
seed: 0
budget:
  max_llm_calls: 100
  max_cost_usd: 5.0
"#,
    );

    let error = load(file.path()).unwrap_err();
    assert!(error.to_string().contains("unknown scenario"));
}

#[test]
fn load_rejects_duplicate_scenario() {
    let file = write_yaml(
        r#"
proxy_listen: "0.0.0.0:7777"
upstream_base_url: "https://api.openai.com"
scenarios: [force_500, force_500]
probability: 0.0
seed: 0
budget:
  max_llm_calls: 100
  max_cost_usd: 5.0
"#,
    );

    let error = load(file.path()).unwrap_err();
    assert!(error.to_string().contains("more than once"));
}

#[test]
fn load_rejects_invalid_probability() {
    let file = write_yaml(
        r#"
proxy_listen: "0.0.0.0:7777"
upstream_base_url: "https://api.openai.com"
scenarios: []
probability: 1.1
seed: 0
budget:
  max_llm_calls: 100
  max_cost_usd: 5.0
"#,
    );

    let error = load(file.path()).unwrap_err();
    assert!(error.to_string().contains("probability"));
}

#[test]
fn load_rejects_zero_call_limit() {
    let file = write_yaml(
        r#"
proxy_listen: "0.0.0.0:7777"
upstream_base_url: "https://api.openai.com"
scenarios: []
probability: 0.0
seed: 0
budget:
  max_llm_calls: 0
  max_cost_usd: 5.0
"#,
    );

    let error = load(file.path()).unwrap_err();
    assert!(error.to_string().contains("greater than zero"));
}

#[test]
fn load_rejects_costs_more_precise_than_micro_usd() {
    let file = write_yaml(
        r#"
proxy_listen: "0.0.0.0:7777"
upstream_base_url: "https://api.openai.com"
scenarios: []
probability: 0.0
seed: 0
budget:
  max_llm_calls: 100
  max_cost_usd: 0.0000001
"#,
    );

    let error = load(file.path()).unwrap_err();
    assert!(error.to_string().contains("more than six decimal places"));
}

#[test]
fn load_rejects_unknown_fields() {
    let file = write_yaml(
        r#"
proxy_listen: "0.0.0.0:7777"
upstream_base_url: "https://api.openai.com"
scenarios: []
probability: 0.0
seed: 0
unexpected: true
budget:
  max_llm_calls: 100
  max_cost_usd: 5.0
"#,
    );

    assert!(load(file.path()).is_err());
}

#[test]
fn load_converts_model_price_overrides_to_micro_usd() {
    let file = write_yaml(
        r#"
proxy_listen: "0.0.0.0:7777"
upstream_base_url: "https://api.openai.com"
scenarios: []
probability: 0.0
seed: 0
budget:
  max_llm_calls: 100
  max_cost_usd: 5.0
model_prices:
  custom-model:
    input_usd_per_million: 0.25
    output_usd_per_million: 1.50
"#,
    );

    let config = load(file.path()).expect("config should load");
    let price = config.model_prices.get("custom-model").expect("price");
    assert_eq!(price.input_per_million, MicroUsd::from_micro_usd(250_000));
    assert_eq!(
        price.output_per_million,
        MicroUsd::from_micro_usd(1_500_000)
    );
}

#[test]
fn load_rejects_negative_model_price() {
    let file = write_yaml(
        r#"
proxy_listen: "0.0.0.0:7777"
upstream_base_url: "https://api.openai.com"
scenarios: []
probability: 0.0
seed: 0
budget:
  max_llm_calls: 100
  max_cost_usd: 5.0
model_prices:
  custom-model:
    input_usd_per_million: -0.25
    output_usd_per_million: 1.50
"#,
    );

    let error = load(file.path()).unwrap_err();
    assert!(error.to_string().contains("non-negative"));
}
