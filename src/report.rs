//! Reliability report collector (actor): events in, JSON out on shutdown.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::{
    budget::{BudgetSnapshot, MicroUsd},
    usage::UsageOutcome,
};

pub const REPORT_SCHEMA_VERSION: &str = "0.1";

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
pub enum BudgetDecision {
    NotBillable,
    InvalidRequest,
    ModelUnpriced,
    Allowed,
    CallCapExceeded,
    CostCapExceeded,
}

#[derive(Debug, Clone)]
pub enum ReportEvent {
    Request {
        record: RequestRecord,
    },
    Shutdown {
        budget_snapshot: Option<BudgetSnapshot>,
        pricing_registry_version: Option<String>,
    },
}

/// Cheap cloneable handle shared via `AppState`.
///
/// Counters are derived at flush from collected records — not mirrored live.
#[derive(Clone)]
pub struct ReportHandle {
    tx: mpsc::UnboundedSender<ReportEvent>,
}

/// Fields collected for one proxy observation before they become a `RequestRecord`.
#[derive(Debug, Clone)]
pub struct RequestObservation {
    pub path: String,
    pub status: u16,
    pub latency_ms: u64,
    pub fault_injected: Option<String>,
    pub billable: bool,
    pub model: Option<String>,
    pub call_number: Option<u64>,
    pub budget_decision: BudgetDecision,
    pub usage: Option<UsageOutcome>,
    pub cost_usd: Option<MicroUsd>,
}

impl RequestObservation {
    pub fn non_billable(
        path: impl Into<String>,
        status: u16,
        latency_ms: u64,
        fault_injected: Option<String>,
    ) -> Self {
        Self {
            path: path.into(),
            status,
            latency_ms,
            fault_injected,
            billable: false,
            model: None,
            call_number: None,
            budget_decision: BudgetDecision::NotBillable,
            usage: None,
            cost_usd: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn billable(
        path: impl Into<String>,
        status: u16,
        latency_ms: u64,
        fault_injected: Option<String>,
        model: Option<String>,
        call_number: Option<u64>,
        budget_decision: BudgetDecision,
        usage: Option<UsageOutcome>,
        cost_usd: Option<MicroUsd>,
    ) -> Self {
        Self {
            path: path.into(),
            status,
            latency_ms,
            fault_injected,
            billable: true,
            model,
            call_number,
            budget_decision,
            usage,
            cost_usd,
        }
    }
}

impl ReportHandle {
    pub fn record(&self, observation: RequestObservation) {
        let _ = self.tx.send(ReportEvent::Request {
            record: RequestRecord {
                path: observation.path,
                status: observation.status,
                latency_ms: observation.latency_ms,
                fault_injected: observation.fault_injected,
                billable: observation.billable,
                model: observation.model,
                call_number: observation.call_number,
                budget_decision: observation.budget_decision,
                usage: observation.usage,
                cost_usd: observation.cost_usd,
            },
        });
    }

    pub fn record_request(
        &self,
        path: impl Into<String>,
        status: u16,
        latency_ms: u64,
        fault_injected: Option<String>,
    ) {
        self.record(RequestObservation::non_billable(
            path,
            status,
            latency_ms,
            fault_injected,
        ));
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_request_details(
        &self,
        path: impl Into<String>,
        status: u16,
        latency_ms: u64,
        fault_injected: Option<String>,
        billable: bool,
        model: Option<String>,
        call_number: Option<u64>,
        budget_decision: BudgetDecision,
        usage: Option<UsageOutcome>,
        cost_usd: Option<MicroUsd>,
    ) {
        let observation = if billable {
            RequestObservation::billable(
                path,
                status,
                latency_ms,
                fault_injected,
                model,
                call_number,
                budget_decision,
                usage,
                cost_usd,
            )
        } else {
            RequestObservation::non_billable(path, status, latency_ms, fault_injected)
        };
        self.record(observation);
    }

    pub fn request_shutdown(&self) {
        self.request_shutdown_with_metadata_inner(None, None);
    }

    pub fn request_shutdown_with_metadata(
        &self,
        budget_snapshot: BudgetSnapshot,
        pricing_registry_version: impl Into<String>,
    ) {
        self.request_shutdown_with_metadata_inner(
            Some(budget_snapshot),
            Some(pricing_registry_version.into()),
        );
    }

    fn request_shutdown_with_metadata_inner(
        &self,
        budget_snapshot: Option<BudgetSnapshot>,
        pricing_registry_version: Option<String>,
    ) {
        let _ = self.tx.send(ReportEvent::Shutdown {
            budget_snapshot,
            pricing_registry_version,
        });
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ReliabilityReport {
    pub schema_version: String,
    pub total_proxy_requests: u64,
    pub billable_llm_calls: u64,
    pub faults_injected: u64,
    pub average_latency_ms: f64,
    pub budget_snapshot: Option<BudgetSnapshot>,
    pub pricing_registry_version: Option<String>,
    pub summary: RunSummary,
    pub requests: Vec<RequestRecord>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RequestRecord {
    pub path: String,
    pub status: u16,
    pub latency_ms: u64,
    pub fault_injected: Option<String>,
    pub billable: bool,
    pub model: Option<String>,
    pub call_number: Option<u64>,
    pub budget_decision: BudgetDecision,
    pub usage: Option<UsageOutcome>,
    pub cost_usd: Option<MicroUsd>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct RunSummary {
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub budget_rejections: u64,
    pub post_fault_successes: u64,
    pub observed_cost_usd: MicroUsd,
}

pub fn summarize(requests: &[RequestRecord]) -> RunSummary {
    let successful_requests = requests
        .iter()
        .filter(|record| (200..300).contains(&record.status))
        .count() as u64;
    let budget_rejections = requests
        .iter()
        .filter(|record| {
            matches!(
                record.budget_decision,
                BudgetDecision::CallCapExceeded | BudgetDecision::CostCapExceeded
            )
        })
        .count() as u64;
    let post_fault_successes = requests
        .windows(2)
        .filter(|records| {
            records[0].fault_injected.is_some() && (200..300).contains(&records[1].status)
        })
        .count() as u64;
    let observed_cost_usd = requests
        .iter()
        .filter_map(|record| record.cost_usd)
        .fold(0u64, |total, cost| total.saturating_add(cost.as_u64()));

    RunSummary {
        successful_requests,
        failed_requests: requests.len() as u64 - successful_requests,
        budget_rejections,
        post_fault_successes,
        observed_cost_usd: MicroUsd::from_micro_usd(observed_cost_usd),
    }
}

/// Spawn the collector task. Shutdown flushes JSON to `output_path`.
pub fn spawn_collector(output_path: impl Into<PathBuf>) -> (ReportHandle, JoinHandle<()>) {
    let output_path = output_path.into();
    let (tx, rx) = mpsc::unbounded_channel();
    let handle = ReportHandle { tx };
    let join = tokio::spawn(async move {
        run_collector(rx, output_path).await;
    });
    (handle, join)
}

async fn run_collector(mut rx: mpsc::UnboundedReceiver<ReportEvent>, output_path: PathBuf) {
    let mut requests = Vec::new();
    let mut total_latency_ms: u64 = 0;
    let mut budget_snapshot = None;
    let mut pricing_registry_version = None;

    while let Some(event) = rx.recv().await {
        match event {
            ReportEvent::Request { record } => {
                total_latency_ms = total_latency_ms.saturating_add(record.latency_ms);
                requests.push(record);
            }
            ReportEvent::Shutdown {
                budget_snapshot: snapshot,
                pricing_registry_version: version,
            } => {
                budget_snapshot = snapshot;
                pricing_registry_version = version;
                break;
            }
        }
    }

    while let Ok(event) = rx.try_recv() {
        if let ReportEvent::Request { record } = event {
            total_latency_ms = total_latency_ms.saturating_add(record.latency_ms);
            requests.push(record);
        }
    }

    let total_proxy_requests = requests.len() as u64;
    let billable_llm_calls = requests
        .iter()
        .filter(|record| record.call_number.is_some())
        .count() as u64;
    let faults_injected = requests
        .iter()
        .filter(|record| record.fault_injected.is_some())
        .count() as u64;
    let average_latency_ms = if total_proxy_requests == 0 {
        0.0
    } else {
        total_latency_ms as f64 / total_proxy_requests as f64
    };
    let summary = summarize(&requests);

    let report = ReliabilityReport {
        schema_version: REPORT_SCHEMA_VERSION.to_owned(),
        total_proxy_requests,
        billable_llm_calls,
        faults_injected,
        average_latency_ms,
        budget_snapshot,
        pricing_registry_version,
        summary,
        requests,
    };

    if let Err(error) = write_report(&output_path, &report) {
        tracing::error!(%error, path = %output_path.display(), "failed to write reliability report");
    } else {
        tracing::info!(
            path = %output_path.display(),
            total_proxy_requests = report.total_proxy_requests,
            billable_llm_calls = report.billable_llm_calls,
            faults_injected = report.faults_injected,
            "wrote reliability_report.json"
        );
    }
}

fn write_report(path: &Path, report: &ReliabilityReport) -> Result<(), Box<dyn std::error::Error>> {
    let file = std::fs::File::create(path)?;
    serde_json::to_writer_pretty(file, report)?;
    Ok(())
}
