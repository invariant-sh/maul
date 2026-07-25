//! Report collector writes reliability_report.json on shutdown.

use maul::report::{ReliabilityReport, spawn_collector};
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
    let report: ReliabilityReport = serde_json::from_str(&raw).expect("json");

    assert_eq!(report.total_calls, 2);
    assert_eq!(report.faults_injected, 1);
    assert_eq!(report.requests.len(), 2);
    assert!(report.average_latency_ms > 0.0);
}
