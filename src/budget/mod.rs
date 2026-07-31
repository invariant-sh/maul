//! Budget-domain primitives shared by configuration, admission, and reporting.

use std::fmt;

use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

use crate::openai::TokenUsage;

/// Monetary amount represented exactly in micro-USD.
#[derive(Debug, Clone, Copy, Default, Eq, Ord, PartialEq, PartialOrd)]
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

impl Serialize for MicroUsd {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("MicroUsd", 2)?;
        state.serialize_field("micro_usd", &self.0)?;
        state.serialize_field("display", &self.to_string())?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for MicroUsd {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct MicroUsdVisitor;

        impl<'de> Visitor<'de> for MicroUsdVisitor {
            type Value = MicroUsd;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a micro-USD integer or {micro_usd, display} object")
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(MicroUsd::from_micro_usd(value))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let value = u64::try_from(value).map_err(E::custom)?;
                Ok(MicroUsd::from_micro_usd(value))
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut micro_usd = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "micro_usd" => {
                            if micro_usd.is_some() {
                                return Err(de::Error::duplicate_field("micro_usd"));
                            }
                            micro_usd = Some(map.next_value::<u64>()?);
                        }
                        "display" => {
                            let _: String = map.next_value()?;
                        }
                        other => {
                            return Err(de::Error::unknown_field(other, &["micro_usd", "display"]));
                        }
                    }
                }
                let micro_usd = micro_usd.ok_or_else(|| de::Error::missing_field("micro_usd"))?;
                Ok(MicroUsd::from_micro_usd(micro_usd))
            }
        }

        deserializer.deserialize_any(MicroUsdVisitor)
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

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct BudgetLimits {
    pub max_llm_calls: u64,
    pub max_cost_usd: MicroUsd,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
pub struct BudgetSnapshot {
    pub calls_reserved: u64,
    pub calls_limit: u64,
    pub observed_cost_usd: MicroUsd,
    pub cost_limit_usd: MicroUsd,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct CallPermit {
    pub call_number: u64,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum BudgetAdmission {
    Allowed(CallPermit),
    CallCapExceeded {
        calls_reserved: u64,
        calls_limit: u64,
    },
    CostCapExceeded {
        observed_cost_usd: MicroUsd,
        cost_limit_usd: MicroUsd,
    },
}

#[derive(Debug)]
struct BudgetState {
    limits: BudgetLimits,
    calls_reserved: AtomicU64,
    observed_cost_usd: AtomicU64,
}

/// Concurrent budget admission and observed-spend accounting.
#[derive(Debug, Clone)]
pub struct BudgetTracker {
    state: Arc<BudgetState>,
}

impl BudgetTracker {
    pub fn new(limits: BudgetLimits) -> Self {
        Self {
            state: Arc::new(BudgetState {
                limits,
                calls_reserved: AtomicU64::new(0),
                observed_cost_usd: AtomicU64::new(0),
            }),
        }
    }

    pub fn admit(&self) -> BudgetAdmission {
        let observed = self.state.observed_cost_usd.load(Ordering::Acquire);
        if self.state.limits.max_cost_usd != MicroUsd::ZERO
            && observed >= self.state.limits.max_cost_usd.as_u64()
        {
            return BudgetAdmission::CostCapExceeded {
                observed_cost_usd: MicroUsd::from_micro_usd(observed),
                cost_limit_usd: self.state.limits.max_cost_usd,
            };
        }

        let call_number = self.reserve_call();
        match call_number {
            Ok(call_number) => BudgetAdmission::Allowed(CallPermit { call_number }),
            Err(calls_reserved) => BudgetAdmission::CallCapExceeded {
                calls_reserved,
                calls_limit: self.state.limits.max_llm_calls,
            },
        }
    }

    pub fn commit_cost(&self, cost: MicroUsd) -> MicroUsd {
        let mut current = self.state.observed_cost_usd.load(Ordering::Acquire);
        loop {
            let next = current.saturating_add(cost.as_u64());
            match self.state.observed_cost_usd.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return MicroUsd::from_micro_usd(next),
                Err(actual) => current = actual,
            }
        }
    }

    pub fn snapshot(&self) -> BudgetSnapshot {
        BudgetSnapshot {
            calls_reserved: self.state.calls_reserved.load(Ordering::Acquire),
            calls_limit: self.state.limits.max_llm_calls,
            observed_cost_usd: MicroUsd::from_micro_usd(
                self.state.observed_cost_usd.load(Ordering::Acquire),
            ),
            cost_limit_usd: self.state.limits.max_cost_usd,
        }
    }

    fn reserve_call(&self) -> Result<u64, u64> {
        let mut current = self.state.calls_reserved.load(Ordering::Acquire);
        loop {
            if current >= self.state.limits.max_llm_calls {
                return Err(current);
            }

            match self.state.calls_reserved.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(current + 1),
                Err(actual) => current = actual,
            }
        }
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum CostError {
    #[error("token cost overflowed micro-USD representation")]
    Overflow,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Price {
    pub input_per_million: MicroUsd,
    pub output_per_million: MicroUsd,
}

impl Price {
    pub const fn new(input_per_million: MicroUsd, output_per_million: MicroUsd) -> Self {
        Self {
            input_per_million,
            output_per_million,
        }
    }

    pub fn calculate(self, usage: &TokenUsage) -> Result<MicroUsd, CostError> {
        let input = u128::from(usage.prompt_tokens)
            .checked_mul(u128::from(self.input_per_million.as_u64()))
            .ok_or(CostError::Overflow)?;
        let output = u128::from(usage.completion_tokens)
            .checked_mul(u128::from(self.output_per_million.as_u64()))
            .ok_or(CostError::Overflow)?;
        let total = input.checked_add(output).ok_or(CostError::Overflow)?;
        let rounded = total.checked_add(500_000).ok_or(CostError::Overflow)? / 1_000_000;
        let value = u64::try_from(rounded).map_err(|_| CostError::Overflow)?;
        Ok(MicroUsd::from_micro_usd(value))
    }
}
