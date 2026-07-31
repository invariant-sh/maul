//! Incremental, bounded SSE usage parser.

use bytes::Bytes;
use futures_util::Stream;
use pin_project_lite::pin_project;
use serde_json::Value;
use std::{
    pin::Pin,
    task::{Context, Poll},
};

use crate::budget::{BudgetTracker, Price};
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

    pub fn finish(&mut self) -> UsageOutcome {
        if self.malformed || self.buffer.len() > self.max_event_bytes {
            return UsageOutcome::Unavailable(UsageUnavailableReason::MalformedSse);
        }
        if !self.buffer.is_empty() {
            self.parse_line(&self.buffer.clone());
        }
        if self.malformed {
            return UsageOutcome::Unavailable(UsageUnavailableReason::MalformedSse);
        }
        self.usage.as_ref().cloned().map_or(
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

pin_project! {
    pub struct SseUsageTap<S> {
        #[pin]
        inner: S,
        parser: SseUsageParser,
        budget: BudgetTracker,
        price: Price,
        finished: bool,
    }
}

impl<S> SseUsageTap<S> {
    pub fn new(inner: S, budget: BudgetTracker, price: Price) -> Self {
        Self {
            inner,
            parser: SseUsageParser::default(),
            budget,
            price,
            finished: false,
        }
    }
}

impl<S, E> Stream for SseUsageTap<S>
where
    S: Stream<Item = Result<Bytes, E>>,
{
    type Item = Result<Bytes, E>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        match this.inner.as_mut().poll_next(context) {
            Poll::Ready(Some(Ok(bytes))) => {
                this.parser.push(&bytes);
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready(Some(Err(error))) => {
                *this.finished = true;
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                if !*this.finished {
                    *this.finished = true;
                    if let UsageOutcome::Metered(usage) = this.parser.finish()
                        && let Ok(cost) = this.price.calculate(&usage)
                    {
                        this.budget.commit_cost(cost);
                    }
                }
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}
