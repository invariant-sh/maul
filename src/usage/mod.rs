//! Bounded token-usage extraction for OpenAI-compatible responses.

pub mod json;
pub mod sse;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::openai::TokenUsage;

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
pub enum UsageUnavailableReason {
    MissingUsage,
    MalformedUsage,
    MalformedSse,
    ResponseTooLarge,
    StreamInterrupted,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub enum UsageOutcome {
    Metered(TokenUsage),
    Unavailable(UsageUnavailableReason),
}

#[derive(Debug, Deserialize)]
pub(crate) struct UsageFields {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

impl TryFrom<UsageFields> for TokenUsage {
    type Error = UsageUnavailableReason;

    fn try_from(value: UsageFields) -> Result<Self, Self::Error> {
        let expected_total = value
            .prompt_tokens
            .checked_add(value.completion_tokens)
            .ok_or(UsageUnavailableReason::MalformedUsage)?;
        if value.total_tokens != expected_total {
            return Err(UsageUnavailableReason::MalformedUsage);
        }
        Ok(Self {
            prompt_tokens: value.prompt_tokens,
            completion_tokens: value.completion_tokens,
            total_tokens: value.total_tokens,
        })
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum RequestTransformError {
    #[error("request body is not a JSON object")]
    NotObject,
    #[error("stream_options must be a JSON object when present")]
    InvalidStreamOptions,
}
