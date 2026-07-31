//! Non-streaming JSON usage extraction.

use serde_json::Value;

use crate::openai::TokenUsage;

use super::{UsageFields, UsageOutcome, UsageUnavailableReason};

pub fn extract_usage(body: &[u8]) -> UsageOutcome {
    let value = match serde_json::from_slice::<Value>(body) {
        Ok(value) => value,
        Err(_) => return UsageOutcome::Unavailable(UsageUnavailableReason::MalformedUsage),
    };
    extract_usage_from_value(&value)
}

pub fn extract_usage_from_value(value: &Value) -> UsageOutcome {
    let usage = match value.get("usage") {
        Some(Value::Object(_)) => {
            match serde_json::from_value::<UsageFields>(value["usage"].clone()) {
                Ok(fields) => fields,
                Err(_) => {
                    return UsageOutcome::Unavailable(UsageUnavailableReason::MalformedUsage);
                }
            }
        }
        Some(Value::Null) | None => {
            return UsageOutcome::Unavailable(UsageUnavailableReason::MissingUsage);
        }
        Some(_) => {
            return UsageOutcome::Unavailable(UsageUnavailableReason::MalformedUsage);
        }
    };

    match TokenUsage::try_from(usage) {
        Ok(usage) => UsageOutcome::Metered(usage),
        Err(reason) => UsageOutcome::Unavailable(reason),
    }
}
