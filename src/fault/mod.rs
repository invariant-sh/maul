//! Fault injection: decide whether to forward, short-circuit, or mutate after upstream.

mod mutate;

pub use mutate::malform_tool_call_json;

use std::sync::Mutex;

use axum::{
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

use crate::{config::Config, openai::OpenAiErrorEnvelope};

pub const FORCE_500: &str = "force_500";
pub const FORCE_429: &str = "force_429";
pub const MALFORMED_TOOL_CALL_JSON: &str = "malformed_tool_call_json";

pub fn is_supported_scenario(name: &str) -> bool {
    matches!(name, FORCE_500 | FORCE_429 | MALFORMED_TOOL_CALL_JSON)
}

/// Control flow for a single proxied request.
#[derive(Debug)]
pub enum Action {
    /// Call the real upstream unchanged.
    Forward,
    /// Return a fabricated response and never call upstream.
    ShortCircuit {
        scenario: &'static str,
        response: Response,
    },
    /// Call upstream, then corrupt the response body before returning to the agent.
    MutateAfter { scenario: &'static str },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scenario {
    Force500,
    Force429,
    MalformedToolCallJson,
}

impl Scenario {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            FORCE_500 => Some(Self::Force500),
            FORCE_429 => Some(Self::Force429),
            MALFORMED_TOOL_CALL_JSON => Some(Self::MalformedToolCallJson),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Force500 => FORCE_500,
            Self::Force429 => FORCE_429,
            Self::MalformedToolCallJson => MALFORMED_TOOL_CALL_JSON,
        }
    }

    fn into_action(self) -> Action {
        match self {
            Self::Force500 => Action::ShortCircuit {
                scenario: FORCE_500,
                response: force_500_response(),
            },
            Self::Force429 => Action::ShortCircuit {
                scenario: FORCE_429,
                response: force_429_response(),
            },
            Self::MalformedToolCallJson => Action::MutateAfter {
                scenario: MALFORMED_TOOL_CALL_JSON,
            },
        }
    }
}

/// Seeded, config-driven fault decisions (safe to share via `Arc`).
pub struct FaultEngine {
    enabled: Vec<Scenario>,
    probability: f64,
    rng: Mutex<StdRng>,
}

impl FaultEngine {
    pub fn from_config(config: &Config) -> Self {
        let enabled = config
            .scenarios
            .iter()
            .filter_map(|name| Scenario::from_name(name))
            .collect();
        let probability = config.probability.clamp(0.0, 1.0);
        Self {
            enabled,
            probability,
            rng: Mutex::new(StdRng::seed_from_u64(config.seed)),
        }
    }

    /// Decide the action for the next request.
    pub fn decide(&self) -> Action {
        if self.enabled.is_empty() || self.probability <= 0.0 {
            return Action::Forward;
        }

        let mut rng = self.rng.lock().expect("fault rng mutex poisoned");
        let roll: f64 = rng.random_range(0.0..1.0);

        if roll >= self.probability {
            return Action::Forward;
        }

        let index = rng.random_range(0..self.enabled.len());
        let scenario = self.enabled[index];
        tracing::warn!(scenario = scenario.name(), roll, "injecting fault");
        scenario.into_action()
    }
}

fn force_500_response() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "maul: injected fault force_500",
    )
        .into_response()
}

fn force_429_response() -> Response {
    let mut response = OpenAiErrorEnvelope::new(
        "maul: injected fault force_429",
        "rate_limit_error",
        FORCE_429,
    )
    .into_response(StatusCode::TOO_MANY_REQUESTS);
    response
        .headers_mut()
        .insert("retry-after", HeaderValue::from_static("1"));
    response
}
