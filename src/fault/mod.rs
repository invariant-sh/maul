//! Fault injection: decide whether to forward, short-circuit, or (later) mutate.

use std::sync::Mutex;

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

use crate::config::Config;

pub const FORCE_500: &str = "force_500";

/// Control flow for a single proxied request.
#[derive(Debug)]
pub enum Action {
    /// Call the real upstream unchanged.
    Forward,
    /// Return a fabricated response and never call upstream.
    ShortCircuit(Response),
}

/// Seeded, config-driven fault decisions (safe to share via `Arc`).
pub struct FaultEngine {
    force_500_enabled: bool,
    probability: f64,
    rng: Mutex<StdRng>,
}

impl FaultEngine {
    pub fn from_config(config: &Config) -> Self {
        let force_500_enabled = config.scenarios.iter().any(|s| s == FORCE_500);
        let probability = config.probability.clamp(0.0, 1.0);
        Self {
            force_500_enabled,
            probability,
            rng: Mutex::new(StdRng::seed_from_u64(config.seed)),
        }
    }

    /// Decide the action for the next request.
    pub fn decide(&self) -> Action {
        if !self.force_500_enabled || self.probability <= 0.0 {
            return Action::Forward;
        }

        let roll: f64 = self
            .rng
            .lock()
            .expect("fault rng mutex poisoned")
            .random_range(0.0..1.0);

        if roll < self.probability {
            tracing::warn!(scenario = FORCE_500, roll, "injecting short-circuit fault");
            Action::ShortCircuit(force_500_response())
        } else {
            Action::Forward
        }
    }
}

fn force_500_response() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "maul: injected fault force_500",
    )
        .into_response()
}
