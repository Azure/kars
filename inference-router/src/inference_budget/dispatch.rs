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
) -> anyhow::Result<(Bytes, Option<AttemptGuard>)> {
    let Some(client) = &upstream.inference_budget else {
        return Ok((body, None));
    };
    let operation = operation(method, path)?;
    let (wire, guard) = client
        .begin(
            upstream.telemetry_provider(),
            &upstream.endpoint,
            &upstream.deployment,
            operation,
            &body,
        )
        .await?;
    Ok((wire, Some(guard)))
}

pub async fn finish(
    guard: Option<AttemptGuard>,
    status: StatusCode,
    body: &[u8],
) -> anyhow::Result<()> {
    if let Some(guard) = guard {
        let usage = status
            .is_success()
            .then(|| usage::buffered(body, guard.quote.contract.operation))
            .flatten();
        let result = guard.finish(usage).await?;
        if result.breach {
            return Err(Error {
                stage: "provider contract breached; account frozen",
                status: None,
            }
            .into());
        }
    }
    Ok(())
}

/// EOF settles complete usage before finishing the client stream. Transport
/// errors and downstream cancellation drop the guard and keep the full maximum.
/// A settlement outage also leaves the durable reservation funded.
pub fn stream(
    inner: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    guard: Option<AttemptGuard>,
    is_sse: bool,
) -> BoxStream<'static, Result<Bytes, reqwest::Error>> {
    let Some(guard) = guard else { return inner };
    let usage = usage::StreamUsage::new(guard.quote.contract.operation);
    futures::stream::unfold(
        (inner, Some(guard), Some(usage), false),
        move |(mut inner, mut guard, mut usage, mut failed)| async move {
            match inner.next().await {
                Some(chunk) => {
                    match &chunk {
                        Ok(bytes) if is_sse && !failed => {
                            if let Some(usage) = &mut usage {
                                usage.push(bytes);
                            }
                        }
                        Err(_) => failed = true,
                        _ => {}
                    }
                    Some((chunk, (inner, guard, usage, failed)))
                }
                None => {
                    if let Some(guard) = guard.take() {
                        let evidence = if is_sse && !failed {
                            usage.take().and_then(usage::StreamUsage::finish)
                        } else {
                            None
                        };
                        match guard.finish(evidence).await {
                            Ok(result) if result.breach => tracing::error!(
                                "Governed inference provider bound breached; account frozen"
                            ),
                            Err(_) => tracing::warn!(
                                "Governed inference settlement pending; reservation remains funded"
                            ),
                            _ => {}
                        }
                    }
                    None
                }
            }
        },
    )
    .boxed()
}
