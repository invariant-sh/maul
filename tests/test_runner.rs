//! Unit tests for `maul test` orchestration helpers.

use maul::budget::{BudgetSnapshot, MicroUsd};
use maul::report::{ReliabilityReport, RunSummary};
use maul::test_runner::{
    AGENT_BASE_URL_ENV, DEFAULT_AGENT_TIMEOUT_SECS, TestOrchestrationError, agent_base_url,
    evaluate_thresholds, publish_report, render_report_summary, write_test_config,
};
use tempfile::tempdir;

fn report_with(
    faults_injected: u64,
    budget_rejections: u64,
    post_fault_successes: u64,
    observed_cost_usd: MicroUsd,
    cost_limit_usd: MicroUsd,
) -> ReliabilityReport {
    ReliabilityReport {
        schema_version: "0.1".into(),
        total_proxy_requests: 2,
        billable_llm_calls: 1,
        faults_injected,
        average_latency_ms: 1.0,
        budget_snapshot: Some(BudgetSnapshot {
            calls_reserved: 1,
            calls_limit: 10,
            observed_cost_usd,
            cost_limit_usd,
        }),
        pricing_registry_version: Some("test".into()),
        summary: RunSummary {
            successful_requests: 1,
            failed_requests: 1,
            budget_rejections,
            post_fault_successes,
            observed_cost_usd,
        },
        requests: Vec::new(),
    }
}

#[test]
fn agent_base_url_uses_openai_compatible_suffix() {
    assert_eq!(agent_base_url("127.0.0.1:9876"), "http://127.0.0.1:9876/v1");
    assert_eq!(AGENT_BASE_URL_ENV, &["MAUL_BASE_URL", "OPENAI_BASE_URL"]);
    assert_eq!(DEFAULT_AGENT_TIMEOUT_SECS, 300);
}

#[test]
fn write_test_config_overrides_proxy_listen() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("source.yaml");
    let destination = dir.path().join("nested/run.yaml");
    std::fs::write(
        &source,
        "proxy_listen: 127.0.0.1:7777\nupstream_base_url: https://api.openai.com/v1\n",
    )
    .unwrap();

    write_test_config(&source, &destination, "127.0.0.1:5555").unwrap();

    let written = std::fs::read_to_string(destination).unwrap();
    assert!(written.contains("127.0.0.1:5555"));
    assert!(!written.contains("127.0.0.1:7777"));
}

#[test]
fn evaluate_thresholds_detects_budget_cost_and_resilience_failures() {
    let budget_fail = report_with(0, 1, 0, MicroUsd::ZERO, MicroUsd::ZERO);
    assert!(matches!(
        evaluate_thresholds(&budget_fail, &["budget".into()]),
        Err(TestOrchestrationError::ThresholdFailed(name)) if name == "budget"
    ));

    let cost_fail = report_with(
        0,
        0,
        0,
        MicroUsd::from_micro_usd(100),
        MicroUsd::from_micro_usd(100),
    );
    assert!(matches!(
        evaluate_thresholds(&cost_fail, &["cost".into()]),
        Err(TestOrchestrationError::ThresholdFailed(name)) if name == "cost"
    ));

    let resilience_fail = report_with(2, 0, 0, MicroUsd::ZERO, MicroUsd::ZERO);
    assert!(matches!(
        evaluate_thresholds(&resilience_fail, &["resilience".into()]),
        Err(TestOrchestrationError::ThresholdFailed(name)) if name == "resilience"
    ));

    let ok = report_with(
        1,
        0,
        1,
        MicroUsd::from_micro_usd(10),
        MicroUsd::from_micro_usd(100),
    );
    assert!(
        evaluate_thresholds(&ok, &["budget".into(), "cost".into(), "resilience".into()]).is_ok()
    );
}

#[test]
fn evaluate_thresholds_rejects_unknown_names() {
    let report = report_with(0, 0, 0, MicroUsd::ZERO, MicroUsd::ZERO);
    assert!(matches!(
        evaluate_thresholds(&report, &["safety".into()]),
        Err(TestOrchestrationError::UnsupportedThreshold(name)) if name == "safety"
    ));
}

#[test]
fn publish_report_writes_json_copy_and_markdown_summary() {
    let dir = tempdir().unwrap();
    let generated = dir.path().join("reliability_report.json");
    let destination = dir.path().join("artifacts/out.json");
    let report = report_with(
        1,
        0,
        1,
        MicroUsd::from_micro_usd(1_500_000),
        MicroUsd::from_micro_usd(5_000_000),
    );
    std::fs::write(&generated, serde_json::to_string_pretty(&report).unwrap()).unwrap();

    let summary_path = publish_report(&generated, &destination).unwrap();
    assert!(destination.exists());
    assert!(summary_path.exists());
    let summary = std::fs::read_to_string(summary_path).unwrap();
    assert!(summary.contains("Faults injected: 1"));
    assert!(summary.contains("$1.500000"));
    assert!(summary.contains("1500000"));
}

#[test]
fn render_report_summary_includes_display_and_micro_amounts() {
    let report = report_with(0, 0, 0, MicroUsd::from_micro_usd(42), MicroUsd::ZERO);
    let summary = render_report_summary(&report);
    assert!(summary.contains("$0.000042"));
    assert!(summary.contains("42"));
}
