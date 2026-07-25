//! Reliability report collector (actor): events in, JSON out on shutdown.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

#[derive(Debug, Clone)]
pub enum ReportEvent {
    Request {
        path: String,
        status: u16,
        latency_ms: u64,
        fault_injected: Option<String>,
    },
    Shutdown,
}

/// Cheap cloneable handle shared via `AppState`.
#[derive(Clone)]
pub struct ReportHandle {
    tx: mpsc::UnboundedSender<ReportEvent>,
    total_calls: Arc<AtomicU64>,
    faults_injected: Arc<AtomicU64>,
}

impl ReportHandle {
    pub fn record_request(
        &self,
        path: impl Into<String>,
        status: u16,
        latency_ms: u64,
        fault_injected: Option<String>,
    ) {
        self.total_calls.fetch_add(1, Ordering::Relaxed);
        if fault_injected.is_some() {
            self.faults_injected.fetch_add(1, Ordering::Relaxed);
        }
        let _ = self.tx.send(ReportEvent::Request {
            path: path.into(),
            status,
            latency_ms,
            fault_injected,
        });
    }

    pub fn request_shutdown(&self) {
        let _ = self.tx.send(ReportEvent::Shutdown);
    }

    pub fn total_calls(&self) -> u64 {
        self.total_calls.load(Ordering::Relaxed)
    }

    pub fn faults_injected(&self) -> u64 {
        self.faults_injected.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReliabilityReport {
    pub total_calls: u64,
    pub faults_injected: u64,
    pub average_latency_ms: f64,
    pub requests: Vec<RequestRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RequestRecord {
    pub path: String,
    pub status: u16,
    pub latency_ms: u64,
    pub fault_injected: Option<String>,
}

/// Spawn the collector task. Drop/`Shutdown` the handle to flush JSON to `output_path`.
pub fn spawn_collector(output_path: impl Into<PathBuf>) -> (ReportHandle, JoinHandle<()>) {
    let output_path = output_path.into();
    let (tx, rx) = mpsc::unbounded_channel();
    let total_calls = Arc::new(AtomicU64::new(0));
    let faults_injected = Arc::new(AtomicU64::new(0));

    let handle = ReportHandle {
        tx,
        total_calls: Arc::clone(&total_calls),
        faults_injected: Arc::clone(&faults_injected),
    };

    let join = tokio::spawn(async move {
        run_collector(rx, output_path).await;
    });

    (handle, join)
}

async fn run_collector(mut rx: mpsc::UnboundedReceiver<ReportEvent>, output_path: PathBuf) {
    let mut requests = Vec::new();
    let mut total_latency_ms: u64 = 0;

    while let Some(event) = rx.recv().await {
        match event {
            ReportEvent::Request {
                path,
                status,
                latency_ms,
                fault_injected,
            } => {
                total_latency_ms = total_latency_ms.saturating_add(latency_ms);
                requests.push(RequestRecord {
                    path,
                    status,
                    latency_ms,
                    fault_injected,
                });
            }
            ReportEvent::Shutdown => break,
        }
    }

    // Drain any events that arrived with/just before Shutdown.
    while let Ok(event) = rx.try_recv() {
        if let ReportEvent::Request {
            path,
            status,
            latency_ms,
            fault_injected,
        } = event
        {
            total_latency_ms = total_latency_ms.saturating_add(latency_ms);
            requests.push(RequestRecord {
                path,
                status,
                latency_ms,
                fault_injected,
            });
        }
    }

    let total_calls = requests.len() as u64;
    let faults_injected = requests
        .iter()
        .filter(|r| r.fault_injected.is_some())
        .count() as u64;
    let average_latency_ms = if total_calls == 0 {
        0.0
    } else {
        total_latency_ms as f64 / total_calls as f64
    };

    let report = ReliabilityReport {
        total_calls,
        faults_injected,
        average_latency_ms,
        requests,
    };

    if let Err(error) = write_report(&output_path, &report) {
        tracing::error!(%error, path = %output_path.display(), "failed to write reliability report");
    } else {
        tracing::info!(
            path = %output_path.display(),
            total_calls = report.total_calls,
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
