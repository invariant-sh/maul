//! Maul — adversarial HTTP proxy between agents and LLM providers.
//!
//! Agent → localhost (Maul) → real OpenAI-compatible base_url.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    Router,
    extract::{Request, State},
    response::Response,
};
use clap::{Parser, Subcommand};
use reqwest::Client;
use serde_yaml::Value;
use thiserror::Error;
use tokio::process::Command;
use tokio::signal;
use tokio::task::JoinError;
use tokio::time::{sleep, timeout};
use tracing_subscriber::EnvFilter;

use maul::budget::{BudgetLimits, BudgetTracker};
use maul::config;
use maul::fault::FaultEngine;
use maul::pricing::PricingRegistry;
use maul::proxy::{self, ProxyState};
use maul::report;

#[derive(Debug, Parser)]
#[command(name = "maul", about = "Adversarial proxy for LLM agent reliability")]
struct Cli {
    #[command(subcommand)]
    command: Option<CommandLine>,
    /// Path to the Maul YAML configuration.
    #[arg(long, value_name = "PATH", default_value = "maul.yaml")]
    config: PathBuf,
    /// Validate configuration and exit without binding a listener.
    #[arg(long)]
    validate: bool,
}

#[derive(Debug, Subcommand)]
enum CommandLine {
    /// Run an agent command against an isolated Maul instance.
    Test(TestArgs),
}

#[derive(Debug, Parser)]
struct TestArgs {
    /// Path to the Maul YAML configuration.
    #[arg(long, value_name = "PATH", default_value = "maul.yaml")]
    config: PathBuf,
    /// Agent command to execute.
    #[arg(long)]
    agent: String,
    /// Destination for the canonical reliability report.
    #[arg(long, value_name = "PATH", default_value = "reliability_report.json")]
    report: PathBuf,
    /// Comma-separated thresholds: budget, cost, resilience.
    #[arg(long, value_delimiter = ',')]
    fail_on: Vec<String>,
    /// Maximum agent runtime in seconds.
    #[arg(long, default_value_t = 300)]
    timeout_secs: u64,
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
    #[error(transparent)]
    Test(#[from] TestError),
}

#[derive(Debug, Error)]
enum TestError {
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Yaml(#[from] serde_yaml::Error),
    #[error("agent command exited unsuccessfully: {0}")]
    AgentFailed(String),
    #[error("agent command timed out after {0} seconds")]
    AgentTimedOut(u64),
    #[error("Maul child process did not become ready")]
    ProxyNotReady,
    #[error("unsupported threshold `{0}`")]
    UnsupportedThreshold(String),
    #[error("reliability threshold failed: {0}")]
    ThresholdFailed(String),
    #[error("reliability report was not produced at `{0}`")]
    MissingReport(String),
    #[error("configuration document must be a YAML mapping")]
    InvalidConfigDocument,
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

async fn run_test(args: TestArgs) -> Result<(), TestError> {
    config::load(&args.config)?;
    let run_dir = temporary_run_directory()?;
    fs::create_dir_all(&run_dir)?;
    let address = reserve_loopback_address()?;
    let config_path = run_dir.join("maul.yaml");
    write_test_config(&args.config, &config_path, &address)?;

    let executable = std::env::current_exe()?;
    let mut proxy = Command::new(executable)
        .arg("--config")
        .arg(&config_path)
        .current_dir(&run_dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;

    if !wait_for_listener(&address).await {
        terminate_child(&mut proxy).await?;
        remove_run_directory(&run_dir);
        return Err(TestError::ProxyNotReady);
    }

    let base_url = format!("http://{address}/v1");
    let mut agent = Command::new("sh")
        .arg("-c")
        .arg(&args.agent)
        .env("MAUL_BASE_URL", &base_url)
        .env("OPENAI_BASE_URL", &base_url)
        .env("MAUL_RUN_ID", format!("maul-{}", std::process::id()))
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;

    let agent_result = match timeout(Duration::from_secs(args.timeout_secs), agent.wait()).await {
        Ok(result) => result?,
        Err(_) => {
            let _ = agent.kill().await;
            terminate_child(&mut proxy).await?;
            remove_run_directory(&run_dir);
            return Err(TestError::AgentTimedOut(args.timeout_secs));
        }
    };
    terminate_child(&mut proxy).await?;

    let generated_report = run_dir.join("reliability_report.json");
    if !generated_report.exists() {
        remove_run_directory(&run_dir);
        return Err(TestError::MissingReport(
            generated_report.display().to_string(),
        ));
    }
    if let Some(parent) = args
        .report
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&generated_report, &args.report)?;
    let report: report::ReliabilityReport =
        serde_json::from_str(&fs::read_to_string(&args.report)?)?;
    let summary_path = args.report.with_extension("md");
    fs::write(&summary_path, render_report_summary(&report))?;
    remove_run_directory(&run_dir);

    if !agent_result.success() {
        return Err(TestError::AgentFailed(format_exit_status(agent_result)));
    }
    evaluate_thresholds(&report, &args.fail_on)?;
    Ok(())
}

fn temporary_run_directory() -> Result<PathBuf, TestError> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    Ok(std::env::temp_dir().join(format!("maul-test-{}-{nanos}", std::process::id())))
}

fn reserve_loopback_address() -> Result<String, TestError> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.to_string())
}

fn write_test_config(source: &Path, destination: &Path, address: &str) -> Result<(), TestError> {
    let contents = fs::read_to_string(source)?;
    let mut yaml: Value = serde_yaml::from_str(&contents)?;
    let mapping = yaml
        .as_mapping_mut()
        .ok_or(TestError::InvalidConfigDocument)?;
    mapping.insert(
        Value::String("proxy_listen".to_owned()),
        Value::String(address.to_owned()),
    );
    fs::write(destination, serde_yaml::to_string(&yaml)?)?;
    Ok(())
}

async fn wait_for_listener(address: &str) -> bool {
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(address).await.is_ok() {
            return true;
        }
        sleep(Duration::from_millis(50)).await;
    }
    false
}

async fn terminate_child(child: &mut tokio::process::Child) -> Result<(), TestError> {
    if let Some(pid) = child.id() {
        #[cfg(unix)]
        {
            let _ = std::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status();
        }
        #[cfg(not(unix))]
        {
            child.kill().await?;
        }
    }

    if timeout(Duration::from_secs(5), child.wait()).await.is_err() {
        child.kill().await?;
        let _ = child.wait().await?;
    }
    Ok(())
}

fn format_exit_status(status: std::process::ExitStatus) -> String {
    status.code().map_or_else(
        || "terminated by signal".to_owned(),
        |code| code.to_string(),
    )
}

fn evaluate_thresholds(
    report: &report::ReliabilityReport,
    thresholds: &[String],
) -> Result<(), TestError> {
    for threshold in thresholds {
        let failed = match threshold.as_str() {
            "budget" => report.summary.budget_rejections > 0,
            "cost" => report
                .budget_snapshot
                .map(|snapshot| {
                    snapshot.cost_limit_usd != maul::budget::MicroUsd::ZERO
                        && report.summary.observed_cost_usd >= snapshot.cost_limit_usd
                })
                .unwrap_or(false),
            "resilience" => report.faults_injected > 0 && report.summary.post_fault_successes == 0,
            unsupported => return Err(TestError::UnsupportedThreshold(unsupported.to_owned())),
        };
        if failed {
            return Err(TestError::ThresholdFailed(threshold.clone()));
        }
    }
    Ok(())
}

fn render_report_summary(report: &report::ReliabilityReport) -> String {
    format!(
        "# Maul test summary\n\n\
         - Schema: `{}`\n\
         - Proxy requests: {}\n\
         - Billable LLM calls: {}\n\
         - Faults injected: {}\n\
         - Budget rejections: {}\n\
         - Observed cost (micro-USD): {}\n",
        report.schema_version,
        report.total_proxy_requests,
        report.billable_llm_calls,
        report.faults_injected,
        report.summary.budget_rejections,
        report.summary.observed_cost_usd.as_u64(),
    )
}

fn remove_run_directory(path: &Path) {
    if let Err(error) = fs::remove_dir_all(path) {
        tracing::debug!(%error, path = %path.display(), "failed to remove temporary test directory");
    }
}

#[tokio::main]
async fn main() -> Result<(), StartupError> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "maul=info".into()))
        .init();
    tracing::info!("starting maul");

    let cli = Cli::parse();
    if let Some(CommandLine::Test(args)) = cli.command {
        return run_test(args).await.map_err(StartupError::from);
    }
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
    let budget = BudgetTracker::new(BudgetLimits {
        max_llm_calls: config.budget.max_llm_calls,
        max_cost_usd: config.budget.max_cost_usd,
    });
    let pricing = PricingRegistry::with_overrides(&config.model_prices);

    let state = ProxyState {
        client,
        upstream_base_url: Arc::new(config.upstream_base_url),
        fault,
        budget,
        pricing,
        report: report.clone(),
    };
    let budget_snapshot = state.budget.snapshot();
    let pricing_registry_version = state.pricing.version().to_owned();

    let app = Router::new().fallback(handler).with_state(state.clone());

    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    report.request_shutdown_with_metadata(budget_snapshot, pricing_registry_version);
    collector.await?;
    Ok(())
}

async fn handler(State(state): State<ProxyState>, req: Request) -> Response {
    proxy::handle(&state, req).await
}
