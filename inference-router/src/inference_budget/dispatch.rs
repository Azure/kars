// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    client::{AttemptGuard, Error},
    usage,
};
use crate::{
    inference_budget_contract::tariffs::Operation, inference_budget_dispatch, proxy::UpstreamConfig,
};
use axum::http::{Method, StatusCode};
use bytes::Bytes;
use futures::{StreamExt, stream::BoxStream};

// Covers the broker RPC and both calendar commit attempts, including time waiting
// for the calendar writer lock. Expiry retains liability; it never starts inference.
const SETTLEMENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
// Two calendar commit attempts plus catalog, session, reserve, and begin RPCs.
const ADMISSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);
const CONSERVATIVE_USAGE_ERROR: &str = "provider usage missing; maximum charge retained";
const RECORD_ONLY_USAGE_ERROR: &str =
    "provider usage missing; record-only calendar pending liability confirmed";
const UNKNOWN_USAGE_ERROR: &str = "provider usage missing; unknown usage retained";

pub struct Attempt {
    settlement: Option<Settlement>,
    operation: Operation,
}

struct Settlement {
    governed: Option<AttemptGuard>,
    calendar: Option<super::calendar::Attempt>,
}

impl Attempt {
    async fn settle(
        mut self,
        evidence: Option<crate::inference_budget_contract::Usage>,
        rejected_before_inference: bool,
    ) -> anyhow::Result<()> {
        let Some(settlement) = self.settlement.take() else {
            return Ok(());
        };
        // Transfer ownership before the first await. Dropping the caller's future
        // detaches the JoinHandle, not either accounting guard or its in-flight RPC.
        settlement
            .spawn(evidence, rejected_before_inference, SETTLEMENT_TIMEOUT)?
            .await
            .map_err(|_| Error {
                stage: "paired settlement worker stopped; liability retained",
                status: None,
                refusal: None,
            })?
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        if let Some(settlement) = self.settlement.take() {
            if settlement.governed.is_some() {
                let _ = settlement.spawn(None, false, SETTLEMENT_TIMEOUT);
            }
            // Without a lifetime receipt, dropping the calendar guard must still
            // latch uncertainty synchronously rather than schedule a zero charge.
        }
    }
}

impl Settlement {
    fn spawn(
        self,
        evidence: Option<crate::inference_budget_contract::Usage>,
        rejected_before_inference: bool,
        timeout: std::time::Duration,
    ) -> anyhow::Result<tokio::task::JoinHandle<anyhow::Result<()>>> {
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| Error {
            stage: "paired settlement runtime unavailable; liability retained",
            status: None,
            refusal: None,
        })?;
        let deadline = tokio::time::Instant::now() + timeout;
        Ok(runtime.spawn(async move {
            let result = match tokio::time::timeout_at(
                deadline,
                self.settle(evidence, rejected_before_inference),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => Err(Error {
                    stage: "paired settlement timeout; liability retained",
                    status: None,
                    refusal: None,
                }
                .into()),
            };
            if let Err(error) = &result {
                tracing::warn!(error = %error, "Inference accounting settlement failed");
            }
            result
        }))
    }

    async fn settle(
        self,
        evidence: Option<crate::inference_budget_contract::Usage>,
        rejected_before_inference: bool,
    ) -> anyhow::Result<()> {
        let record_only = self
            .governed
            .as_ref()
            .is_none_or(|guard| guard.quote.is_record_only());
        let maximum = self
            .governed
            .as_ref()
            .and_then(|guard| guard.quote.bounded())
            .map(|quote| quote.maximum);
        let governed = if let Some(guard) = self.governed {
            Some(guard.finish(evidence.clone()).await)
        } else {
            None
        };
        let conservative_charge = governed
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .filter(|result| {
                evidence.is_none()
                    && result.finalized
                    && result.uncertain
                    && !result.breach
                    && result.charged == maximum
            })
            .and_then(|result| result.charged)
            .map(|charge| charge.tokens);
        if let Some(guard) = self.calendar {
            let tokens = evidence
                .and_then(|value| value.input_tokens.checked_add(value.output_tokens))
                .or_else(|| rejected_before_inference.then_some(0))
                .or(conservative_charge);
            match tokens {
                Some(tokens) => guard.finish(tokens).await?,
                None => {
                    if record_only
                        && governed.as_ref().is_none_or(|result| {
                            result.as_ref().is_ok_and(|result| {
                                result.finalized && result.uncertain && !result.breach
                            })
                        })
                    {
                        guard.confirm_record_only_pending().await?;
                        return Err(Error {
                            stage: RECORD_ONLY_USAGE_ERROR,
                            status: None,
                            refusal: None,
                        }
                        .into());
                    }
                    drop(guard);
                    return Err(super::calendar::Error(
                        "incomplete provider usage; pending liability retained",
                    )
                    .into());
                }
            }
            if conservative_charge.is_some() && !rejected_before_inference {
                return Err(Error {
                    stage: CONSERVATIVE_USAGE_ERROR,
                    status: None,
                    refusal: None,
                }
                .into());
            }
        }
        if let Some(result) = governed {
            let result = result?;
            if record_only && result.uncertain && !result.breach && !rejected_before_inference {
                return Err(Error {
                    stage: UNKNOWN_USAGE_ERROR,
                    status: None,
                    refusal: None,
                }
                .into());
            }
            if result.breach {
                return Err(Error {
                    stage: "provider contract breached; account frozen",
                    status: None,
                    refusal: None,
                }
                .into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Arc, time::Duration};
    use tokio::sync::Notify;
    use wiremock::{Mock, Request, ResponseTemplate};

    #[tokio::test]
    async fn paired_settlement_deadline_includes_calendar_lock_wait() {
        let (server, calendar) = super::super::calendar::tests::accounting_server().await;
        let guard = calendar.begin().await.unwrap();
        let received = Arc::new(Notify::new());
        let notify = received.clone();
        Mock::given(wiremock::matchers::path("/v1/calendar/read"))
            .respond_with(move |_: &Request| {
                notify.notify_one();
                ResponseTemplate::new(503).set_delay(Duration::from_secs(5))
            })
            .with_priority(1)
            .mount(&server)
            .await;
        let reader = calendar.clone();
        let blocked_read = tokio::spawn(async move { reader.read().await });
        tokio::time::timeout(Duration::from_secs(5), received.notified())
            .await
            .unwrap();
        let worker = Settlement {
            governed: None,
            calendar: Some(guard),
        }
        .spawn(
            usage::buffered(
                br#"{"usage":{"input_tokens":4,"output_tokens":5,"total_tokens":9}}"#,
                Operation::Responses,
            ),
            false,
            Duration::from_millis(20),
        )
        .unwrap();
        let error = tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .expect("settlement worker exceeded its deadline")
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("paired settlement timeout"));
        blocked_read.abort();
        assert!(blocked_read.await.unwrap_err().is_cancelled());
        assert!(calendar.read().await.is_err());
        assert!(calendar.begin().await.is_err());
        let commits = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|request| request.url.path() == "/v1/calendar/commit")
            .count();
        assert_eq!(commits, 1, "only Begin: expiry must not fabricate a Finish");
    }
}

fn operation(method: &Method, path: &str) -> Result<Operation, Error> {
    let denied = || Error {
        stage: "unsupported inference operation",
        status: None,
        refusal: None,
    };
    if method != Method::POST {
        return Err(denied());
    }
    inference_budget_dispatch::operation(path).ok_or_else(denied)
}

/// Classify only: unsupported finite routes must not consult unrelated provider
/// credentials first. Supported sends still acquire their grant at final dispatch.
pub(crate) fn preflight(
    upstream: &UpstreamConfig,
    method: &Method,
    path: &str,
) -> Result<(), Error> {
    if upstream.inference_budget.is_some() {
        operation(method, path)?;
    }
    Ok(())
}

/// Runs after provider resolution and all wire transformations, immediately
/// before the actual transport send. Each retry needs a separate grant.
pub async fn begin(
    upstream: &UpstreamConfig,
    method: &Method,
    path: &str,
    body: Bytes,
) -> anyhow::Result<(Bytes, Option<Attempt>)> {
    if upstream.inference_budget.is_none() && upstream.calendar_budget.is_none() {
        return Ok((body, None));
    }
    let runtime = tokio::runtime::Handle::try_current().map_err(|_| Error {
        stage: "admission runtime unavailable",
        status: None,
        refusal: None,
    })?;
    let upstream = upstream.clone();
    let method = method.clone();
    let path = path.to_owned();
    // A disconnected caller must not interrupt the calendar/lifetime handshake.
    // This worker never sends to the provider. An abandoned granted Attempt still
    // uses its normal conservative settlement; a refusal closes the calendar at zero.
    runtime
        .spawn(async move {
            tokio::time::timeout(
                ADMISSION_TIMEOUT,
                begin_accounting(&upstream, &method, &path, body),
            )
            .await
            .map_err(|_| Error {
                stage: "admission timeout; liability retained",
                status: None,
                refusal: None,
            })?
        })
        .await
        .map_err(|_| Error {
            stage: "admission worker stopped; liability retained",
            status: None,
            refusal: None,
        })?
}

async fn begin_accounting(
    upstream: &UpstreamConfig,
    method: &Method,
    path: &str,
    body: Bytes,
) -> anyhow::Result<(Bytes, Option<Attempt>)> {
    let operation = match operation(method, path) {
        Ok(operation) => operation,
        Err(error) if upstream.inference_budget.is_some() => return Err(error.into()),
        Err(_) => return Ok((body, None)),
    };
    let calendar = if let Some(client) = &upstream.calendar_budget {
        Some(client.begin().await?)
    } else {
        None
    };
    let (wire, governed) = if let Some(client) = &upstream.inference_budget {
        match client
            .begin(
                upstream.telemetry_provider(),
                &upstream.endpoint,
                &upstream.deployment,
                operation,
                &body,
            )
            .await
        {
            Ok((wire, guard)) => (wire, Some(guard)),
            Err(error) => {
                if let Some(guard) = calendar {
                    guard.finish(0).await?;
                }
                return Err(error.into());
            }
        }
    } else {
        (body, None)
    };
    Ok((
        wire,
        Some(Attempt {
            settlement: Some(Settlement { governed, calendar }),
            operation,
        }),
    ))
}

fn provider_error_parameter(status: StatusCode, body: &[u8]) -> &'static str {
    if status.is_success() || body.len() > 65_536 {
        return "unavailable";
    }
    let parsed = serde_json::from_slice::<serde_json::Value>(body).ok();
    match parsed
        .as_ref()
        .and_then(|value| value.pointer("/error/param"))
        .and_then(|value| value.as_str())
    {
        Some("tool_choice") => "tool_choice",
        Some("tools") => "tools",
        Some("parallel_tool_calls") => "parallel_tool_calls",
        Some("max_tokens") => "max_tokens",
        Some("max_completion_tokens") => "max_completion_tokens",
        Some("messages") => "messages",
        Some("model") => "model",
        Some("temperature") => "temperature",
        Some("top_p") => "top_p",
        Some("reasoning_effort") => "reasoning_effort",
        Some("response_format") => "response_format",
        Some("stream_options") => "stream_options",
        Some(_) => "other",
        None => "unavailable",
    }
}

fn provider_content_filter_rejection(status: StatusCode, body: &[u8]) -> bool {
    if !matches!(status, StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN) || body.len() > 65_536 {
        return false;
    }
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/code")
                .and_then(|code| code.as_str())
                .map(|code| matches!(code, "content_filter" | "responsible_ai_policy_violation"))
        })
        .unwrap_or(false)
}

fn provider_parameter_rejection(status: StatusCode, body: &[u8]) -> bool {
    if status != StatusCode::BAD_REQUEST || body.len() > 65_536 {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    // Provider error codes are optional and provider-specific, not usage evidence.
    value
        .pointer("/error/code")
        .is_none_or(|code| code.is_null() || code.is_string())
        && value
            .pointer("/error/param")
            .and_then(|param| param.as_str())
            .is_some_and(|param| !param.trim().is_empty())
}

pub async fn finish(guard: Option<Attempt>, status: StatusCode, body: &[u8]) -> anyhow::Result<()> {
    if let Some(guard) = guard {
        let usage = status
            .is_success()
            .then(|| usage::buffered(body, guard.operation))
            .flatten();
        let rejected = matches!(status, StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND)
            && serde_json::from_slice::<serde_json::Value>(body)
                .ok()
                .and_then(|body| {
                    body.pointer("/error/code")
                        .and_then(|v| v.as_str())
                        .map(str::to_ascii_lowercase)
                })
                .is_some_and(|code| {
                    matches!(
                        code.as_str(),
                        "unsupported_api_for_model"
                            | "model_not_found"
                            | "deploymentnotfound"
                            | "model_not_supported"
                            | "invalid_model"
                    )
                });
        let content_filter_rejection = provider_content_filter_rejection(status, body);
        if usage.is_none() {
            // Settlement may replace the upstream status with a local accounting error.
            // Record only fixed classifications, never provider messages or response bodies.
            tracing::warn!(
                upstream_status = status.as_u16(),
                provider_error_parameter = provider_error_parameter(status, body),
                provider_content_filter_rejection = content_filter_rejection,
                rejected_before_inference = rejected,
                "Upstream response lacks verifiable usage; applying existing settlement policy"
            );
        }
        match guard.settle(usage, rejected).await {
            // Preserve refusals and retryable provider failures only after acknowledged
            // conservative settlement or confirmed record-only uncertainty.
            Err(error)
                if (content_filter_rejection
                    || provider_parameter_rejection(status, body)
                    || crate::proxy::failure::retryable_rejection(status))
                    && error.downcast_ref::<Error>().is_some_and(|error| {
                        matches!(
                            error.stage,
                            CONSERVATIVE_USAGE_ERROR
                                | RECORD_ONLY_USAGE_ERROR
                                | UNKNOWN_USAGE_ERROR
                        ) && error.status.is_none()
                            && error.refusal.is_none()
                    }) =>
            {
                tracing::info!(
                    upstream_status = status.as_u16(),
                    accounting = %error,
                    "Preserving provider error response with retained accounting liability"
                );
            }
            result => result?,
        }
    }
    Ok(())
}

/// Settle trustworthy final usage before exposing the terminal event.
/// Cancellation settles conservatively only with an acknowledged lifetime charge;
/// otherwise cancellation and incomplete usage retain durable calendar liability.
pub fn stream(
    inner: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    guard: Option<Attempt>,
    is_sse: bool,
) -> BoxStream<'static, Result<Bytes, std::io::Error>> {
    let inner = inner
        .map(|chunk| chunk.map_err(std::io::Error::other))
        .boxed();
    let Some(guard) = guard else { return inner };
    let operation = guard.operation;
    let usage = usage::StreamUsage::new(operation);
    futures::stream::unfold(
        (inner, Some(guard), Some(usage), false, Some(Vec::new())),
        move |(mut inner, mut guard, mut usage, mut failed, mut buffered)| async move {
            match inner.next().await {
                Some(mut chunk) => {
                    match &chunk {
                        Ok(bytes) if is_sse && !failed => {
                            if let Some(usage) = &mut usage {
                                usage.push(bytes);
                            }
                        }
                        Ok(bytes) if !is_sse && !failed => {
                            if let Some(buffer) = &mut buffered {
                                if buffer.len().saturating_add(bytes.len()) <= 1_048_576 {
                                    buffer.extend_from_slice(bytes);
                                } else {
                                    buffered = None;
                                }
                            }
                        }
                        Err(_) => failed = true,
                        _ => {}
                    }
                    if let Some(parser) =
                        usage.as_ref().filter(|parser| parser.finished_or_invalid())
                        && let Some(attempt) = guard.take()
                        && let Err(error) = attempt.settle(parser.complete(), false).await
                    {
                        failed = true;
                        chunk = Err(std::io::Error::other(error));
                    }
                    Some((chunk, (inner, guard, usage, failed, buffered)))
                }
                None => {
                    if let Some(guard) = guard.take() {
                        let evidence = if is_sse && !failed {
                            usage.take().and_then(usage::StreamUsage::finish)
                        } else if !failed {
                            buffered
                                .as_deref()
                                .and_then(|body| usage::buffered(body, operation))
                        } else {
                            None
                        };
                        if let Err(error) = guard.settle(evidence, false).await {
                            tracing::warn!(error = %error,
                                "Inference settlement unresolved; durable liability retained");
                            return Some((
                                Err(std::io::Error::other(error)),
                                (inner, None, usage, true, buffered),
                            ));
                        }
                    }
                    None
                }
            }
        },
    )
    .boxed()
}
