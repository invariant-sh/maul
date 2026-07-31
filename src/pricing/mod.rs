//! Versioned model pricing registry.

use std::collections::HashMap;

use crate::budget::{MicroUsd, Price};

pub const REGISTRY_VERSION: &str = "openai-text-2025-05";

#[derive(Debug, Clone)]
pub struct PricingRegistry {
    prices: HashMap<String, Price>,
}

impl PricingRegistry {
    pub fn with_overrides(overrides: &HashMap<String, Price>) -> Self {
        let mut prices = builtin_prices();
        prices.extend(overrides.clone());
        Self { prices }
    }

    pub fn lookup(&self, model: &str) -> Option<Price> {
        self.prices.get(model).copied()
    }

    pub const fn version(&self) -> &'static str {
        REGISTRY_VERSION
    }
}

fn builtin_prices() -> HashMap<String, Price> {
    HashMap::from([
        (
            "gpt-4o-mini".to_owned(),
            Price::new(
                MicroUsd::from_micro_usd(150_000),
                MicroUsd::from_micro_usd(600_000),
            ),
        ),
        (
            "gpt-4o".to_owned(),
            Price::new(
                MicroUsd::from_micro_usd(2_500_000),
                MicroUsd::from_micro_usd(10_000_000),
            ),
        ),
        (
            "gpt-4.1".to_owned(),
            Price::new(
                MicroUsd::from_micro_usd(2_000_000),
                MicroUsd::from_micro_usd(8_000_000),
            ),
        ),
        (
            "gpt-4.1-mini".to_owned(),
            Price::new(
                MicroUsd::from_micro_usd(400_000),
                MicroUsd::from_micro_usd(1_600_000),
            ),
        ),
        (
            "gpt-4.1-nano".to_owned(),
            Price::new(
                MicroUsd::from_micro_usd(100_000),
                MicroUsd::from_micro_usd(400_000),
            ),
        ),
    ])
}
