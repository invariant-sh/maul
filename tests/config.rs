//! Tests for YAML config loading.

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
                max_cost_usd: 1.5,
            },
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
