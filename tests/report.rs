//! Report collector writes reliability_report.json on shutdown.

use maul::report::{BudgetDecision, ReliabilityReport, spawn_collector};
use std::time::Duration;
use tempfile::tempdir;

#[tokio::test]
async fn flush_writes_json_report() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("reliability_report.json");

    let (handle, collector) = spawn_collector(path.clone());
    handle.record_request("/v1/chat/completions", 200, 12, None);
    handle.record_request("/v1/chat/completions", 500, 3, Some("force_500".into()));
    handle.request_shutdown();

    tokio::time::timeout(Duration::from_secs(2), collector)
        .await
        .expect("collector finished")
        .expect("collector join");

    let raw = std::fs::read_to_string(&path).expect("report file");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("json value");
    let report: ReliabilityReport = serde_json::from_str(&raw).expect("json");

    assert_eq!(value["schema_version"], "0.2");
    assert_eq!(
        value["summary"]["recovery_events"],
        value["summary"]["post_fault_successes"]
    );

    assert_eq!(report.schema_version, "0.2");
    assert!(!report.run_id.is_empty());
    assert_eq!(report.total_proxy_requests, 2);
    assert_eq!(report.billable_llm_calls, 0);
    assert_eq!(report.faults_injected, 1);
    assert_eq!(report.requests.len(), 2);
    assert_eq!(report.requests[0].sequence, 1);
    assert_eq!(report.requests[1].sequence, 2);
    assert_eq!(report.summary.failed_requests, 1);
    assert!(report.average_latency_ms > 0.0);
}

#[tokio::test]
async fn flush_preserves_typed_budget_and_summary_fields() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("reliability_report.json");
    let (handle, collector) = spawn_collector(path.clone());

    handle.record_request_details(
        "/v1/chat/completions",
        200,
        5,
        None,
        true,
        Some("gpt-4o-mini".to_owned()),
        Some(1),
        BudgetDecision::Allowed,
        None,
        None,
        None,
    );
    handle.request_shutdown();
    tokio::time::timeout(Duration::from_secs(2), collector)
        .await
        .expect("collector finished")
        .expect("collector join");

    let raw = std::fs::read_to_string(path).expect("report file");
    let report: ReliabilityReport = serde_json::from_str(&raw).expect("json");
    assert_eq!(report.billable_llm_calls, 1);
    assert_eq!(report.requests[0].call_number, Some(1));
    assert_eq!(report.requests[0].budget_decision, BudgetDecision::Allowed);
}

#[tokio::test]
async fn flush_dual_serializes_micro_usd_amounts() {
    use maul::budget::MicroUsd;
    use maul::report::BudgetDecision;

    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("reliability_report.json");
    let (handle, collector) = spawn_collector(path.clone());

    handle.record_request_details(
        "/v1/chat/completions",
        200,
        5,
        None,
        true,
        Some("gpt-4o-mini".to_owned()),
        Some(1),
        BudgetDecision::Allowed,
        None,
        Some(MicroUsd::from_micro_usd(1_500_000)),
        None,
    );
    handle.request_shutdown_with_metadata(
        maul::budget::BudgetSnapshot {
            calls_reserved: 1,
            calls_limit: 10,
            observed_cost_usd: MicroUsd::from_micro_usd(1_500_000),
            cost_limit_usd: MicroUsd::from_micro_usd(5_000_000),
        },
        "bundled-v1",
    );
    tokio::time::timeout(Duration::from_secs(2), collector)
        .await
        .expect("collector finished")
        .expect("collector join");

    let raw = std::fs::read_to_string(path).expect("report file");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("json");
    assert_eq!(
        value["summary"]["observed_cost_usd"]["micro_usd"],
        1_500_000
    );
    assert_eq!(
        value["summary"]["observed_cost_usd"]["display"],
        "$1.500000"
    );
    let report: ReliabilityReport = serde_json::from_str(&raw).expect("typed");
    assert_eq!(
        report.summary.observed_cost_usd,
        MicroUsd::from_micro_usd(1_500_000)
    );
}

#[test]
fn schema_v01_fixtures_remain_deserializable() {
    let raw = r#"{
        "schema_version": "0.1",
        "total_proxy_requests": 2,
        "billable_llm_calls": 1,
        "faults_injected": 1,
        "average_latency_ms": 10.0,
        "budget_snapshot": null,
        "pricing_registry_version": null,
        "summary": {
            "successful_requests": 1,
            "failed_requests": 1,
            "budget_rejections": 0,
            "post_fault_successes": 5,
            "observed_cost_usd": { "micro_usd": 0, "display": "$0.000000" }
        },
        "requests": [
            {
                "path": "/v1/chat/completions",
                "status": 500,
                "latency_ms": 3,
                "fault_injected": "force_500",
                "billable": true,
                "model": "gpt-4o-mini",
                "call_number": 1,
                "budget_decision": "Allowed",
                "usage": null,
                "cost_usd": null
            }
        ]
    }"#;
    let report: ReliabilityReport = serde_json::from_str(raw).expect("0.1 fixture");
    assert_eq!(report.schema_version, "0.1");
    assert!(report.run_id.is_empty());
    assert_eq!(report.summary.post_fault_successes, 5);
    assert_eq!(report.summary.recovery_events, 0);
    assert!(report.requests[0].session_id.is_none());
    assert_eq!(report.requests[0].sequence, 0);
    assert!(report.sessions.is_empty());
}
