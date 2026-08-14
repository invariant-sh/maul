//! Reliability report collector (actor): events in, JSON out on shutdown.
//!
//! Schema `0.2` is additive over `0.1`: new session fields default on deserialize
//! so existing fixtures remain readable. Newly emitted reports always use `0.2`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::{
    budget::{BudgetSnapshot, MicroUsd},
    usage::UsageOutcome,
};

pub const REPORT_SCHEMA_VERSION: &str = "0.2";
pub const REPORT_SCHEMA_VERSION_V01: &str = "0.1";

const FORCE_500: &str = "force_500";
const FORCE_429: &str = "force_429";
const MALFORMED_TOOL_CALL_JSON: &str = "malformed_tool_call_json";

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
///
/// This type stays free of HTTP/framework types. Session attribution is an
/// already-validated identifier string, or `None` for unattributed traffic.
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
    pub session_id: Option<String>,
}

impl RequestObservation {
    pub fn non_billable(
        path: impl Into<String>,
        status: u16,
        latency_ms: u64,
        fault_injected: Option<String>,
        session_id: Option<String>,
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
            session_id,
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
        session_id: Option<String>,
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
            session_id,
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
                session_id: observation.session_id,
                sequence: 0,
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
        self.record_request_with_session(path, status, latency_ms, fault_injected, None);
    }

    pub fn record_request_with_session(
        &self,
        path: impl Into<String>,
        status: u16,
        latency_ms: u64,
        fault_injected: Option<String>,
        session_id: Option<String>,
    ) {
        self.record(RequestObservation::non_billable(
            path,
            status,
            latency_ms,
            fault_injected,
            session_id,
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
        session_id: Option<String>,
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
                session_id,
            )
        } else {
            RequestObservation::non_billable(path, status, latency_ms, fault_injected, session_id)
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
    #[serde(default)]
    pub run_id: String,
    pub total_proxy_requests: u64,
    pub billable_llm_calls: u64,
    pub faults_injected: u64,
    pub average_latency_ms: f64,
    pub budget_snapshot: Option<BudgetSnapshot>,
    pub pricing_registry_version: Option<String>,
    pub summary: RunSummary,
    #[serde(default)]
    pub sessions: Vec<SessionSummary>,
    pub requests: Vec<RequestRecord>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
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
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub sequence: u64,
}

impl Default for RequestRecord {
    fn default() -> Self {
        Self {
            path: "/v1/chat/completions".to_owned(),
            status: 200,
            latency_ms: 1,
            fault_injected: None,
            billable: true,
            model: None,
            call_number: None,
            budget_decision: BudgetDecision::Allowed,
            usage: None,
            cost_usd: None,
            session_id: None,
            sequence: 0,
        }
    }
}

/// Per-session recovery counters. Sessions with no faults have `recovered: None`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub request_count: u64,
    pub fault_events: u64,
    pub recovery_events: u64,
    pub recovered: Option<bool>,
}

#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct RunSummary {
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub budget_rejections: u64,
    /// Canonical count of recovered fault events (same-session later success).
    #[serde(default)]
    pub recovery_events: u64,
    /// Compatibility alias of `recovery_events` for schema `0.1` consumers.
    pub post_fault_successes: u64,
    #[serde(default)]
    pub fault_events: u64,
    #[serde(default)]
    pub sessions_observed: u64,
    #[serde(default)]
    pub recovered_sessions: u64,
    #[serde(default)]
    pub unrecovered_sessions: u64,
    #[serde(default)]
    pub unattributed_requests: u64,
    pub observed_cost_usd: MicroUsd,
}

/// Derived session-aware recovery totals. Recovery is never inferred for
/// unattributed traffic.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionAggregation {
    pub sessions: Vec<SessionSummary>,
    pub recovery_events: u64,
    pub fault_events: u64,
    pub attributed_fault_events: u64,
    pub sessions_observed: u64,
    pub recovered_sessions: u64,
    pub unrecovered_sessions: u64,
    pub unattributed_requests: u64,
}

pub fn summarize(requests: &[RequestRecord]) -> RunSummary {
    let successful_requests = requests
        .iter()
        .filter(|record| is_success_status(record.status))
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
    let aggregation = aggregate_sessions(requests);
    let observed_cost_usd = requests
        .iter()
        .filter_map(|record| record.cost_usd)
        .fold(0u64, |total, cost| total.saturating_add(cost.as_u64()));

    RunSummary {
        successful_requests,
        failed_requests: requests.len() as u64 - successful_requests,
        budget_rejections,
        recovery_events: aggregation.recovery_events,
        post_fault_successes: aggregation.recovery_events,
        fault_events: aggregation.fault_events,
        sessions_observed: aggregation.sessions_observed,
        recovered_sessions: aggregation.recovered_sessions,
        unrecovered_sessions: aggregation.unrecovered_sessions,
        unattributed_requests: aggregation.unattributed_requests,
        observed_cost_usd: MicroUsd::from_micro_usd(observed_cost_usd),
    }
}

/// Group ordered proxy events by session and count recoveries.
///
/// A fault event is recovered only when a later request in the **same** session
/// is a qualifying success. `force_500` / `force_429` accept any later 2xx.
/// `malformed_tool_call_json` requires a later un-faulted 2xx. Unattributed
/// requests never create inferred recovery.
pub fn aggregate_sessions(requests: &[RequestRecord]) -> SessionAggregation {
    let mut order: Vec<String> = Vec::new();
    let mut grouped: HashMap<String, Vec<&RequestRecord>> = HashMap::new();
    let mut unattributed_requests = 0;

    for record in requests {
        match record.session_id.as_deref() {
            Some(session_id) => {
                if !grouped.contains_key(session_id) {
                    order.push(session_id.to_owned());
                }
                grouped
                    .entry(session_id.to_owned())
                    .or_default()
                    .push(record);
            }
            None => unattributed_requests += 1,
        }
    }

    let sessions: Vec<SessionSummary> = order
        .into_iter()
        .filter_map(|session_id| {
            grouped
                .remove(&session_id)
                .map(|events| summarize_session(session_id, &events))
        })
        .collect();

    let attributed_fault_events: u64 = sessions.iter().map(|session| session.fault_events).sum();
    let recovery_events: u64 = sessions.iter().map(|session| session.recovery_events).sum();
    let recovered_sessions = sessions
        .iter()
        .filter(|session| session.recovered == Some(true))
        .count() as u64;
    let unrecovered_sessions = sessions
        .iter()
        .filter(|session| session.recovered == Some(false))
        .count() as u64;
    let unattributed_faults = requests
        .iter()
        .filter(|record| record.session_id.is_none() && record.fault_injected.is_some())
        .count() as u64;

    SessionAggregation {
        sessions_observed: sessions.len() as u64,
        recovered_sessions,
        unrecovered_sessions,
        unattributed_requests,
        fault_events: attributed_fault_events.saturating_add(unattributed_faults),
        attributed_fault_events,
        recovery_events,
        sessions,
    }
}

fn summarize_session(session_id: String, events: &[&RequestRecord]) -> SessionSummary {
    let fault_indices: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(index, record)| record.fault_injected.as_ref().map(|_| index))
        .collect();
    let recovery_events = fault_indices
        .iter()
        .filter(|&&fault_index| {
            let scenario = events[fault_index]
                .fault_injected
                .as_deref()
                .expect("fault index refers to a faulted record");
            events[fault_index + 1..]
                .iter()
                .any(|later| is_qualifying_recovery(scenario, later))
        })
        .count() as u64;
    let fault_events = fault_indices.len() as u64;
    let recovered = if fault_events == 0 {
        None
    } else {
        Some(recovery_events == fault_events)
    };

    SessionSummary {
        session_id,
        request_count: events.len() as u64,
        fault_events,
        recovery_events,
        recovered,
    }
}

/// Whether `later` can recover a prior injected-fault episode of `scenario`.
pub fn is_qualifying_recovery(scenario: &str, later: &RequestRecord) -> bool {
    if !is_success_status(later.status) {
        return false;
    }
    match scenario {
        FORCE_500 | FORCE_429 => true,
        MALFORMED_TOOL_CALL_JSON => later.fault_injected.is_none(),
        _ => later.fault_injected.is_none(),
    }
}

fn is_success_status(status: u16) -> bool {
    (200..300).contains(&status)
}

pub fn resolve_run_id() -> String {
    std::env::var("MAUL_RUN_ID")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty() && value.len() <= 128)
        .unwrap_or_else(generate_run_id)
}

fn generate_run_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    format!("maul-{}-{millis}", std::process::id())
}

/// Spawn the collector task. Shutdown flushes JSON to `output_path`.
pub fn spawn_collector(output_path: impl Into<PathBuf>) -> (ReportHandle, JoinHandle<()>) {
    let output_path = output_path.into();
    let run_id = resolve_run_id();
    let (tx, rx) = mpsc::unbounded_channel();
    let handle = ReportHandle { tx };
    let join = tokio::spawn(async move {
        run_collector(rx, output_path, run_id).await;
    });
    (handle, join)
}

async fn run_collector(
    mut rx: mpsc::UnboundedReceiver<ReportEvent>,
    output_path: PathBuf,
    run_id: String,
) {
    let mut requests = Vec::new();
    let mut total_latency_ms: u64 = 0;
    let mut budget_snapshot = None;
    let mut pricing_registry_version = None;
    let mut next_sequence = 1u64;

    while let Some(event) = rx.recv().await {
        match event {
            ReportEvent::Request { mut record } => {
                record.sequence = next_sequence;
                next_sequence = next_sequence.saturating_add(1);
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
        if let ReportEvent::Request { mut record } = event {
            record.sequence = next_sequence;
            next_sequence = next_sequence.saturating_add(1);
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
    let sessions = aggregate_sessions(&requests).sessions;

    let report = ReliabilityReport {
        schema_version: REPORT_SCHEMA_VERSION.to_owned(),
        run_id,
        total_proxy_requests,
        billable_llm_calls,
        faults_injected,
        average_latency_ms,
        budget_snapshot,
        pricing_registry_version,
        summary,
        sessions,
        requests,
    };

    if let Err(error) = write_report(&output_path, &report) {
        tracing::error!(%error, path = %output_path.display(), "failed to write reliability report");
    } else {
        tracing::info!(
            path = %output_path.display(),
            run_id = %report.run_id,
            total_proxy_requests = report.total_proxy_requests,
            billable_llm_calls = report.billable_llm_calls,
            faults_injected = report.faults_injected,
            unrecovered_sessions = report.summary.unrecovered_sessions,
            "wrote reliability_report.json"
        );
    }
}

fn write_report(path: &Path, report: &ReliabilityReport) -> Result<(), Box<dyn std::error::Error>> {
    let file = std::fs::File::create(path)?;
    serde_json::to_writer_pretty(file, report)?;
    Ok(())
}
