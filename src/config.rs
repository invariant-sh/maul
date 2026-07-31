use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
};

use rust_decimal::Decimal;
use serde::Deserialize;
use thiserror::Error;

use crate::{
    budget::{MicroUsd, MicroUsdError, Price},
    fault::is_supported_scenario,
    openai::{ModelId, ModelIdError},
};

const DEFAULT_PATH: &str = "maul.yaml";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawBudget {
    pub max_llm_calls: u64,
    pub max_cost_usd: Decimal,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawPrice {
    pub input_usd_per_million: Decimal,
    pub output_usd_per_million: Decimal,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawConfig {
    pub proxy_listen: String,
    pub upstream_base_url: String,
    pub scenarios: Vec<String>,
    pub probability: f64,
    pub seed: u64,
    pub budget: RawBudget,
    #[serde(default)]
    pub model_prices: HashMap<String, RawPrice>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Budget {
    pub max_llm_calls: u64,
    pub max_cost_usd: MicroUsd,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub proxy_listen: String,
    pub upstream_base_url: String,
    pub scenarios: Vec<String>,
    pub probability: f64,
    pub seed: u64,
    pub budget: Budget,
    pub model_prices: HashMap<String, Price>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config `{path}`: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse config: {0}")]
    Parse(#[from] serde_yaml::Error),
    #[error("proxy_listen cannot be empty")]
    EmptyListenAddress,
    #[error("upstream_base_url cannot be empty")]
    EmptyUpstreamUrl,
    #[error("probability must be finite and between 0.0 and 1.0, got {0}")]
    InvalidProbability(f64),
    #[error("budget.max_llm_calls must be greater than zero")]
    InvalidCallLimit,
    #[error("unknown scenario `{0}`")]
    UnknownScenario(String),
    #[error("scenario `{0}` is configured more than once")]
    DuplicateScenario(String),
    #[error("invalid max_cost_usd: {0}")]
    InvalidCost(#[from] MicroUsdError),
    #[error("invalid model identifier `{model}`: {source}")]
    InvalidModelId {
        model: String,
        #[source]
        source: ModelIdError,
    },
}

impl RawConfig {
    pub fn validate(self) -> Result<Config, ConfigError> {
        if self.proxy_listen.trim().is_empty() {
            return Err(ConfigError::EmptyListenAddress);
        }
        if self.upstream_base_url.trim().is_empty() {
            return Err(ConfigError::EmptyUpstreamUrl);
        }
        if !self.probability.is_finite() || !(0.0..=1.0).contains(&self.probability) {
            return Err(ConfigError::InvalidProbability(self.probability));
        }
        if self.budget.max_llm_calls == 0 {
            return Err(ConfigError::InvalidCallLimit);
        }

        let mut seen = HashSet::with_capacity(self.scenarios.len());
        for scenario in &self.scenarios {
            if !is_supported_scenario(scenario) {
                return Err(ConfigError::UnknownScenario(scenario.clone()));
            }
            if !seen.insert(scenario) {
                return Err(ConfigError::DuplicateScenario(scenario.clone()));
            }
        }

        let model_prices = self
            .model_prices
            .into_iter()
            .map(|(model, raw_price)| {
                let model_id = ModelId::try_from(model.clone()).map_err(|source| {
                    ConfigError::InvalidModelId {
                        model: model.clone(),
                        source,
                    }
                })?;
                let input = MicroUsd::try_from(raw_price.input_usd_per_million)?;
                let output = MicroUsd::try_from(raw_price.output_usd_per_million)?;
                Ok((model_id.as_str().to_owned(), Price::new(input, output)))
            })
            .collect::<Result<HashMap<_, _>, ConfigError>>()?;

        Ok(Config {
            proxy_listen: self.proxy_listen,
            upstream_base_url: self.upstream_base_url,
            scenarios: self.scenarios,
            probability: self.probability,
            seed: self.seed,
            budget: Budget {
                max_llm_calls: self.budget.max_llm_calls,
                max_cost_usd: MicroUsd::try_from(self.budget.max_cost_usd)?,
            },
            model_prices,
        })
    }
}

pub fn load_default() -> Result<Config, ConfigError> {
    load(DEFAULT_PATH)
}

pub fn load(path: impl AsRef<Path>) -> Result<Config, ConfigError> {
    tracing::info!("Loading config");

    let path = path.as_ref();
    let file_content = fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.display().to_string(),
        source,
    })?;
    let raw = serde_yaml::from_str::<RawConfig>(&file_content)?;
    raw.validate()
}
