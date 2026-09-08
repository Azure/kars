// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    TaskTelemetry,
    parse::{self, Parsed, Shape},
};
use bytes::Bytes;
use futures::{Stream, stream::BoxStream};
use serde_json::json;
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Instant,
};

mod stream;

pub struct Observation {
    telemetry: Arc<TaskTelemetry>,
    scope: String,
    round: u64,
    shape: Shape,
    provider: Option<String>,
    model: Option<String>,
    started: Instant,
    status: Option<u16>,
    finished: bool,
}
impl Observation {
    pub(super) fn new(
        telemetry: Arc<TaskTelemetry>,
        scope: String,
        round: u64,
        shape: Shape,
        provider: Option<String>,
        model: Option<String>,
    ) -> Self {
        Self {
            telemetry,
            scope,
            round,
            shape,
            provider,
            model,
            started: Instant::now(),
            status: None,
            finished: false,
        }
    }
    pub fn headers(&mut self, status: u16) {
        self.status = Some(status);
    }
    pub fn fail(&mut self, outcome: &str) {
        self.finish_parsed(
            outcome,
            Parsed {
                partial: true,
                ..Default::default()
            },
        );
    }
    pub fn buffered(&mut self, status: u16, body: &[u8]) {
        self.headers(status);
        let parsed = if body.len() <= parse::MAX_BODY {
            serde_json::from_slice(body)
                .ok()
                .map(|value| parse::response(&value, self.shape))
        } else {
            None
        };
        self.finish_parsed(
            if status < 400 {
                "complete"
            } else {
                "http_error"
            },
            parsed.unwrap_or(Parsed {
                partial: true,
                ..Default::default()
            }),
        );
    }
    fn finish_parsed(&mut self, outcome: &str, parsed: Parsed) {
        if self.finished {
            return;
        }
        self.finished = true;
        let usage_state = match (parsed.usage.prompt_tokens, parsed.usage.completion_tokens) {
            (Some(_), Some(_)) => "present",
            (None, None) => "missing",
            _ => "partial",
        };
        let accepted = if matches!(outcome, "configuration_error" | "authentication_error") {
            Some(false)
        } else {
            self.status.map(|status| (200..300).contains(&status))
        };
        let event = json!({"kind":"round", "round":self.round, "source":"router-upstream",
            "provider":self.provider, "model":self.model, "http_status":self.status,
            "accepted":accepted,
            "outcome":outcome, "usage":parsed.usage, "usage_state":usage_state,
            "finish_reason":parsed.finish, "partial_observation":parsed.partial,
            "ms":self.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            "tool_calls_observed":parsed.tools.len(),
        });
        self.telemetry
            .finish(&self.scope, self.round, event, parsed.tools);
    }
}
impl Drop for Observation {
    fn drop(&mut self) {
        if !self.finished {
            self.fail("cancelled");
        }
    }
}

pub fn wrap_stream(
    inner: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    observation: Option<Observation>,
    is_sse: bool,
) -> BoxStream<'static, Result<Bytes, reqwest::Error>> {
    match observation {
        Some(observation) => {
            let accumulator = stream::Accumulator::new(observation.shape, is_sse);
            Box::pin(ObservedStream {
                inner,
                observation,
                accumulator,
            })
        }
        None => inner,
    }
}

struct ObservedStream {
    inner: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    observation: Observation,
    accumulator: stream::Accumulator,
}
impl Drop for ObservedStream {
    fn drop(&mut self) {
        if !self.observation.finished {
            let parsed = self.accumulator.finish();
            self.observation.finish_parsed("cancelled", parsed);
        }
    }
}
impl Stream for ObservedStream {
    type Item = Result<Bytes, reqwest::Error>;
    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.as_mut().poll_next(context) {
            Poll::Ready(Some(Ok(bytes))) => {
                self.accumulator.feed(&bytes);
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready(Some(Err(error))) => {
                let parsed = self.accumulator.finish();
                self.observation
                    .finish_parsed("response_body_error", parsed);
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                let parsed = self.accumulator.finish();
                let outcome = if self.observation.status.is_some_and(|status| status >= 400) {
                    "http_error"
                } else if self.accumulator.failed {
                    "upstream_error"
                } else if self.accumulator.complete() {
                    "complete"
                } else {
                    "incomplete"
                };
                self.observation.finish_parsed(outcome, parsed);
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}
