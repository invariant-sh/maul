//! OpenAI-compatible protocol types and route classification.

use axum::{
    Json,
    http::{Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const CHAT_COMPLETIONS_PATH: &str = "/v1/chat/completions";

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum BillableRoute {
    ChatCompletions,
}

pub fn classify_billable_route(method: &Method, uri: &Uri) -> Option<BillableRoute> {
    if method != Method::POST {
        return None;
    }

    match uri.path() {
        CHAT_COMPLETIONS_PATH | "/v1/chat/completions/" => Some(BillableRoute::ChatCompletions),
        _ => None,
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ModelId(String);

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ModelIdError {
    #[error("model identifier cannot be empty")]
    Empty,
    #[error("model identifier cannot contain whitespace")]
    Whitespace,
}

impl TryFrom<String> for ModelId {
    type Error = ModelIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() {
            return Err(ModelIdError::Empty);
        }
        if value.chars().any(char::is_whitespace) {
            return Err(ModelIdError::Whitespace);
        }
        Ok(Self(value))
    }
}

impl ModelId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ChatRequestMetadata {
    pub model: ModelId,
    pub stream: bool,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ChatRequestError {
    #[error("chat completion request must be a JSON object")]
    NotObject,
    #[error("chat completion request is missing a model")]
    MissingModel,
    #[error(transparent)]
    InvalidModel(#[from] ModelIdError),
}

impl TryFrom<&Value> for ChatRequestMetadata {
    type Error = ChatRequestError;

    fn try_from(value: &Value) -> Result<Self, Self::Error> {
        let object = value.as_object().ok_or(ChatRequestError::NotObject)?;
        let model = object
            .get("model")
            .and_then(Value::as_str)
            .ok_or(ChatRequestError::MissingModel)?;
        Ok(Self {
            model: ModelId::try_from(model.to_owned())?,
            stream: object
                .get("stream")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct OpenAiError {
    pub message: String,
    #[serde(rename = "type")]
    pub error_type: String,
    pub code: String,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct OpenAiErrorEnvelope {
    pub error: OpenAiError,
}

impl OpenAiErrorEnvelope {
    pub fn new(
        message: impl Into<String>,
        error_type: impl Into<String>,
        code: impl Into<String>,
    ) -> Self {
        Self {
            error: OpenAiError {
                message: message.into(),
                error_type: error_type.into(),
                code: code.into(),
            },
        }
    }

    pub fn into_response(self, status: StatusCode) -> Response {
        (status, Json(self)).into_response()
    }
}
