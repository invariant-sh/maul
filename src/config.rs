use serde::Deserialize;
use std::fs;

const DEFAULT_PATH: &str = "maul.yaml";

#[derive(Debug, Deserialize, PartialEq)]
pub struct Budget {
    pub max_llm_calls: u64,
    pub max_cost_usd: f64,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct Config {
    pub proxy_listen: String,
    pub upstream_base_url: String,
    pub scenarios: Vec<String>,
    pub probability: f64,
    pub seed: u64,
    pub budget: Budget,
}

pub fn load_default() -> Result<Config, Box<dyn std::error::Error>> {
    load(DEFAULT_PATH)
}

pub fn load(path: &str) -> Result<Config, Box<dyn std::error::Error>> {
    tracing::info!("Loading config");

    let file_content = fs::read_to_string(path)?;
    let config = serde_yaml::from_str(&file_content)?;
    Ok(config)
}
