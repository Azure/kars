// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use futures::{StreamExt, TryStreamExt, stream::BoxStream};

type StreamingResult = (
    StatusCode,
    HeaderMap,
    BoxStream<'static, Result<Bytes, reqwest::Error>>,
    UpstreamConfig,
);

/// Fail over only before a successful response is accepted. A successful stream
/// is never replayed, even if its first body chunk subsequently fails.
#[allow(clippy::too_many_arguments)]
pub async fn forward_stream_with_failover(
    auth: Arc<WorkloadIdentityAuth>,
    copilot: Option<Arc<CopilotTokenCache>>,
    client: Client,
    health: &Arc<DeploymentHealthRegistry>,
    upstream_base: &UpstreamConfig,
    config: &Config,
    snapshot: &InferencePolicySnapshot,
    path: &str,
    request_headers: HeaderMap,
    request_body: Bytes,
) -> Result<StreamingResult> {
    let candidates = candidates_for_request(upstream_base, snapshot, &request_body);
    let mut eligible: Vec<_> = candidates
        .iter()
        .filter(|candidate| health.is_healthy(&health_key(candidate)))
        .collect();
    if eligible.is_empty() {
        eligible.push(&candidates[0]);
    }
    let mut last_result = None;
    for candidate in eligible {
        let key = health_key(candidate);
        let upstream = resolve_candidate(upstream_base, config, candidate);
        let body = request_body_for_candidate(&request_body, &candidate.deployment);
        let attempt = crate::proxy::forward_stream(
            auth.clone(),
            copilot.clone(),
            client.clone(),
            upstream.clone(),
            path,
            request_headers.clone(),
            body,
        )
        .await;
        match attempt {
            Ok((status, headers, stream)) if is_failover_trigger(status) => {
                health.record_failure(&key);
                tracing::warn!(provider = %key, status = %status, digest = %snapshot.digest, "streaming inference failover");
                let buffered = stream
                    .try_fold(Vec::new(), |mut bytes, chunk| async move {
                        bytes.extend_from_slice(&chunk);
                        Ok(bytes)
                    })
                    .await;
                last_result = Some(match buffered {
                    Ok(bytes) => {
                        let stream =
                            futures::stream::once(async move { Ok(Bytes::from(bytes)) }).boxed();
                        Ok((status, headers, stream, upstream))
                    }
                    Err(error) => Err(error.into()),
                });
            }
            Ok((status, headers, stream)) => {
                if status.is_success() {
                    health.record_success(&key);
                }
                return Ok((status, headers, stream, upstream));
            }
            Err(error) => {
                health.record_failure(&key);
                last_result = Some(Err(error));
            }
        }
    }
    last_result.expect("at least one candidate is attempted")
}
