//! Budget-domain primitives shared by configuration, admission, and reporting.

use std::fmt;

use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Monetary amount represented exactly in micro-USD.
#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct MicroUsd(u64);

impl MicroUsd {
    pub const ZERO: Self = Self(0);

    pub const fn from_micro_usd(value: u64) -> Self {
        Self(value)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl fmt::Display for MicroUsd {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "${}.{:06}",
            self.0 / 1_000_000,
            self.0 % 1_000_000
        )
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MicroUsdError {
    #[error("amount must be non-negative")]
    Negative,
    #[error("amount has more than six decimal places: {0}")]
    TooPrecise(Decimal),
    #[error("amount is too large for micro-USD representation: {0}")]
    Overflow(Decimal),
}

impl TryFrom<Decimal> for MicroUsd {
    type Error = MicroUsdError;

    fn try_from(amount: Decimal) -> Result<Self, Self::Error> {
        if amount.is_sign_negative() {
            return Err(MicroUsdError::Negative);
        }

        let scaled = amount * Decimal::from(1_000_000u64);
        if !scaled.fract().is_zero() {
            return Err(MicroUsdError::TooPrecise(amount));
        }

        scaled
            .to_u64()
            .map(Self)
            .ok_or(MicroUsdError::Overflow(amount))
    }
}
