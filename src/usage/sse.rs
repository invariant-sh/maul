//! Incremental, bounded SSE usage parser.

use serde_json::Value;

use crate::openai::TokenUsage;

use super::{UsageFields, UsageOutcome, UsageUnavailableReason};

pub const DEFAULT_MAX_EVENT_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub struct SseUsageParser {
    buffer: Vec<u8>,
    max_event_bytes: usize,
    usage: Option<TokenUsage>,
    malformed: bool,
}

impl SseUsageParser {
    pub fn new(max_event_bytes: usize) -> Self {
        Self {
            buffer: Vec::new(),
            max_event_bytes,
            usage: None,
            malformed: false,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        if self.malformed {
            return;
        }
        self.buffer.extend_from_slice(chunk);
        if self.buffer.len() > self.max_event_bytes && !self.buffer.contains(&b'\n') {
            self.malformed = true;
            return;
        }

        while let Some(position) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line = self.buffer.drain(..=position).collect::<Vec<_>>();
            self.parse_line(&line[..line.len() - 1]);
            if self.malformed {
                return;
            }
        }
    }

    pub fn finish(mut self) -> UsageOutcome {
        if self.malformed || self.buffer.len() > self.max_event_bytes {
            return UsageOutcome::Unavailable(UsageUnavailableReason::MalformedSse);
        }
        if !self.buffer.is_empty() {
            self.parse_line(&self.buffer.clone());
        }
        if self.malformed {
            return UsageOutcome::Unavailable(UsageUnavailableReason::MalformedSse);
        }
        self.usage.map_or(
            UsageOutcome::Unavailable(UsageUnavailableReason::MissingUsage),
            UsageOutcome::Metered,
        )
    }

    fn parse_line(&mut self, line: &[u8]) {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let text = match std::str::from_utf8(line) {
            Ok(text) => text.trim_start(),
            Err(_) => {
                self.malformed = true;
                return;
            }
        };
        let Some(payload) = text.strip_prefix("data:") else {
            return;
        };
        let payload = payload.trim();
        if payload.is_empty() || payload == "[DONE]" {
            return;
        }

        let value = match serde_json::from_str::<Value>(payload) {
            Ok(value) => value,
            Err(_) => {
                self.malformed = true;
                return;
            }
        };
        let Some(usage) = value.get("usage") else {
            return;
        };
        if usage.is_null() {
            return;
        }

        let fields = match serde_json::from_value::<UsageFields>(usage.clone()) {
            Ok(fields) => fields,
            Err(_) => {
                self.malformed = true;
                return;
            }
        };
        self.usage = TokenUsage::try_from(fields).ok();
        if self.usage.is_none() {
            self.malformed = true;
        }
    }
}

impl Default for SseUsageParser {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_EVENT_BYTES)
    }
}
