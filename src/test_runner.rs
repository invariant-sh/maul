//! `maul test` orchestration helpers shared by the binary and unit tests.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_yaml::Value;
use thiserror::Error;

use crate::budget::MicroUsd;
use crate::report::{ReliabilityReport, aggregate_sessions};

/// Environment variables injected so framework-agnostic agents can target Maul.
pub const AGENT_BASE_URL_ENV: &[&str] = &["MAUL_BASE_URL", "OPENAI_BASE_URL"];

/// Run identifier shared by the proxy report and the agent process.
pub const AGENT_RUN_ENV: &str = "MAUL_RUN_ID";

/// Session identifier agents should send as `X-Maul-Session-Id` (or JSON `user`).
pub const AGENT_SESSION_ENV: &str = "MAUL_SESSION_ID";

/// Default `--timeout-secs` for `maul test` agent processes.
pub const DEFAULT_AGENT_TIMEOUT_SECS: u64 = 300;

#[derive(Debug, Error)]
pub enum TestOrchestrationError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Yaml(#[from] serde_yaml::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("unsupported threshold `{0}`")]
    UnsupportedThreshold(String),
    #[error("{name} threshold failed: {detail}")]
    ThresholdFailed { name: String, detail: String },
    #[error("configuration document must be a YAML mapping")]
    InvalidConfigDocument,
}

/// Override `proxy_listen` so the test runner can bind an ephemeral loopback port.
pub fn write_test_config(
    source: &Path,
    destination: &Path,
    address: &str,
) -> Result<(), TestOrchestrationError> {
    let contents = fs::read_to_string(source)?;
    let mut yaml: Value = serde_yaml::from_str(&contents)?;
    let mapping = yaml
        .as_mapping_mut()
        .ok_or(TestOrchestrationError::InvalidConfigDocument)?;
    mapping.insert(
        Value::String("proxy_listen".to_owned()),
        Value::String(address.to_owned()),
    );
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(destination, serde_yaml::to_string(&yaml)?)?;
    Ok(())
}

/// Evaluate `--fail-on` thresholds against a flushed reliability report.
pub fn evaluate_thresholds(
    report: &ReliabilityReport,
    thresholds: &[String],
) -> Result<(), TestOrchestrationError> {
    for threshold in thresholds {
        let failed = match threshold.as_str() {
            "budget" => report.summary.budget_rejections > 0,
            "cost" => report
                .budget_snapshot
                .map(|snapshot| {
                    snapshot.cost_limit_usd != MicroUsd::ZERO
                        && report.summary.observed_cost_usd >= snapshot.cost_limit_usd
                })
                .unwrap_or(false),
            "resilience" => {
                let aggregation = aggregate_sessions(&report.requests);
                aggregation.unrecovered_sessions > 0
            }
            unsupported => {
                return Err(TestOrchestrationError::UnsupportedThreshold(
                    unsupported.to_owned(),
                ));
            }
        };
        if failed {
            let detail = threshold_detail(threshold, report);
            return Err(TestOrchestrationError::ThresholdFailed {
                name: threshold.clone(),
                detail,
            });
        }
    }
    Ok(())
}

fn threshold_detail(threshold: &str, report: &ReliabilityReport) -> String {
    match threshold {
        "budget" => format!(
            "{} budget rejection(s) recorded",
            report.summary.budget_rejections
        ),
        "cost" => format!(
            "observed cost {} reached the configured limit {}",
            report.summary.observed_cost_usd,
            report
                .budget_snapshot
                .map(|snapshot| snapshot.cost_limit_usd.to_string())
                .unwrap_or_else(|| "unknown".to_owned())
        ),
        "resilience" => {
            let failed = aggregate_sessions(&report.requests).unrecovered_sessions;
            format!("{failed} faulted session(s) had no later same-session recovery")
        }
        other => format!("threshold `{other}` failed"),
    }
}

/// Markdown summary written next to the JSON report for CI logs.
pub fn render_report_summary(report: &ReliabilityReport) -> String {
    format!(
        "# Maul test summary\n\n\
         - Schema: `{}`\n\
         - Run ID: `{}`\n\
         - Proxy requests: {}\n\
         - Billable LLM calls: {}\n\
         - Fault events: {}\n\
         - Recovery events: {} (alias: post_fault_successes)\n\
         - Recovered sessions: {}\n\
         - Unrecovered sessions: {}\n\
         - Unattributed requests: {}\n\
         - Budget rejections: {}\n\
         - Observed cost: {} (`{}` micro-USD)\n",
        report.schema_version,
        report.run_id,
        report.total_proxy_requests,
        report.billable_llm_calls,
        report.summary.fault_events,
        report.summary.recovery_events,
        report.summary.recovered_sessions,
        report.summary.unrecovered_sessions,
        report.summary.unattributed_requests,
        report.summary.budget_rejections,
        report.summary.observed_cost_usd,
        report.summary.observed_cost_usd.as_u64(),
    )
}

/// Build the OpenAI-compatible base URL exposed to the agent process.
pub fn agent_base_url(listen_address: &str) -> String {
    format!("http://{listen_address}/v1")
}

/// Copy the generated report into the caller-requested destination.
pub fn publish_report(
    generated_report: &Path,
    destination: &Path,
) -> Result<PathBuf, TestOrchestrationError> {
    if let Some(parent) = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    fs::copy(generated_report, destination)?;
    let summary_path = destination.with_extension("md");
    let report: ReliabilityReport = serde_json::from_str(&fs::read_to_string(destination)?)?;
    fs::write(&summary_path, render_report_summary(&report))?;
    Ok(summary_path)
}
