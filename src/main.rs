//! Maul — adversarial HTTP proxy between agents and LLM providers.
//!
//! Agent → localhost (Maul) → real OpenAI-compatible base_url.

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    extract::{Request, State},
    response::Response,
};
use reqwest::Client;
use tokio::signal;
use tracing_subscriber::EnvFilter;

use maul::config;
use maul::fault::FaultEngine;
use maul::proxy::{self, ProxyState};
use maul::report;

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
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "maul=info".into()))
        .init();
    tracing::info!("starting maul");

    let config = config::load_default()?;
    tracing::info!(
        seed = config.seed,
        probability = config.probability,
        scenarios = ?config.scenarios,
        max_llm_calls = config.budget.max_llm_calls,
        max_cost_usd = config.budget.max_cost_usd,
        "config loaded"
    );

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
