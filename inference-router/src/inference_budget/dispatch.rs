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

pub struct Attempt {
    governed: Option<AttemptGuard>,
    calendar: Option<super::calendar::Attempt>,
    operation: Operation,
}

impl Attempt {
    async fn settle(
        self,
        evidence: Option<crate::inference_budget_contract::Usage>,
        rejected_before_inference: bool,
    ) -> anyhow::Result<()> {
        let governed = if let Some(guard) = self.governed {
            Some(guard.finish(evidence.clone()).await)
        } else {
            None
        };
        if let Some(guard) = self.calendar {
            let tokens = evidence
                .and_then(|value| value.input_tokens.checked_add(value.output_tokens))
                .or_else(|| rejected_before_inference.then_some(0));
            match tokens {
                Some(tokens) => guard.finish(tokens).await?,
                None => {
                    drop(guard);
                    return Err(super::calendar::Error(
                        "incomplete provider usage; pending liability retained",
                    )
                    .into());
                }
            }
        }
        if let Some(result) = governed {
            if result?.breach {
                return Err(Error {
                    stage: "provider contract breached; account frozen",
                    status: None,
                }
                .into());
            }
        }
        Ok(())
    }
}

fn operation(method: &Method, path: &str) -> Result<Operation, Error> {
    let denied = || Error {
        stage: "unsupported inference operation",
        status: None,
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
            governed,
            calendar,
            operation,
        }),
    ))
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
        guard.settle(usage, rejected).await?;
    }
    Ok(())
}

/// Settle trustworthy final usage before exposing the terminal event.
/// Cancellation and incomplete usage retain durable liability.
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
