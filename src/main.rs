//! Maul — adversarial HTTP proxy between agents and LLM providers.
//!
//! Agent → localhost (Maul) → real OpenAI-compatible base_url.

use std::{path::PathBuf, sync::Arc, time::Duration};

use axum::{
    Router,
    extract::{Request, State},
    response::Response,
};
use clap::Parser;
use reqwest::Client;
use thiserror::Error;
use tokio::signal;
use tokio::task::JoinError;
use tracing_subscriber::EnvFilter;

use maul::config;
use maul::fault::FaultEngine;
use maul::proxy::{self, ProxyState};
use maul::report;

#[derive(Debug, Parser)]
#[command(name = "maul", about = "Adversarial proxy for LLM agent reliability")]
struct Cli {
    /// Path to the Maul YAML configuration.
    #[arg(long, value_name = "PATH", default_value = "maul.yaml")]
    config: PathBuf,
    /// Validate configuration and exit without binding a listener.
    #[arg(long)]
    validate: bool,
}

#[derive(Debug, Error)]
enum StartupError {
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error("failed to build HTTP client: {0}")]
    HttpClient(#[from] reqwest::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("collector task failed: {0}")]
    Collector(#[from] JoinError),
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutting down");
}

#[tokio::main]
async fn main() -> Result<(), StartupError> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "maul=info".into()))
        .init();
    tracing::info!("starting maul");

    let cli = Cli::parse();
    let config = config::load(&cli.config)?;
    tracing::info!(
        seed = config.seed,
        probability = config.probability,
        scenarios = ?config.scenarios,
        max_llm_calls = config.budget.max_llm_calls,
        max_cost_usd = %config.budget.max_cost_usd,
        "config loaded"
    );

    if cli.validate {
        tracing::info!(path = %cli.config.display(), "configuration is valid");
        return Ok(());
    }

    let listen_addr = config.proxy_listen.clone();
    tracing::info!(%listen_addr, "listening");

    let client = Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(60))
        .build()?;

    let (report, collector) = report::spawn_collector("reliability_report.json");
    let fault = Arc::new(FaultEngine::from_config(&config));

    let state = ProxyState {
        client,
        upstream_base_url: Arc::new(config.upstream_base_url),
        fault,
        report: report.clone(),
    };

    let app = Router::new().fallback(handler).with_state(state);

    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    report.request_shutdown();
    collector.await?;
    Ok(())
}

async fn handler(State(state): State<ProxyState>, req: Request) -> Response {
    proxy::handle(&state, req).await
}
