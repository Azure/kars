// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{AppState, memory_access::AuthorizedMemoryRequest};
use crate::providers::{AuditEvent, audit_now_ms};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};

pub async fn authorize(
    state: &AppState,
    request: Option<&AuthorizedMemoryRequest>,
) -> Result<Option<String>, Response> {
    let Some(budget) = &state.inference_budget else {
        return Ok(None);
    };
    let digest = match request {
        Some(request) => request.current_authorization_digest(state).await,
        None => None,
    };
    if let Some(digest) = digest {
        match budget.allows_bound_memory(&state.sandbox_name).await {
            Ok(true) => return Ok(Some(digest)),
            Ok(false) => {}
            Err(_) => return Err(unavailable("Memory funding authority is unavailable")),
        }
    }
    Err(crate::errors::openai_coded(
        StatusCode::FORBIDDEN,
        "This Foundry operation is outside the closed governed-inference contract; memory requires an authorized private partition or live binding and unbounded funding without a monetary ceiling",
        "unsupported_budget_operation",
        "unsupported_budget_operation",
    ).into_response())
}

fn unavailable(message: &'static str) -> Response {
    crate::errors::openai_coded(
        StatusCode::SERVICE_UNAVAILABLE,
        message,
        "memory_accounting_unavailable",
        "memory_accounting_unavailable",
    )
    .into_response()
}

pub struct Observation {
    id: String,
    digest: String,
    binding: String,
    operation: String,
}

impl Observation {
    pub async fn begin(
        state: &AppState,
        binding: Option<String>,
        operation: &str,
        body: &[u8],
    ) -> Result<Option<Self>, Response> {
        let Some(binding) = binding else {
            return Ok(None);
        };
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let observation = Self {
            id: format!(
                "{}-{}",
                audit_now_ms(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ),
            digest: format!("{:x}", Sha256::digest(body)),
            binding,
            operation: operation.into(),
        };
        observation
            .append(state, "dispatch_intent", None, &[])
            .await
            .map_err(|_| {
                unavailable("Memory dispatch audit could not be recorded; request was not sent")
            })?;
        Ok(Some(observation))
    }

    async fn append(
        &self,
        state: &AppState,
        phase: &str,
        status: Option<StatusCode>,
        body: &[u8],
    ) -> Result<(), crate::providers::AuditError> {
        let usage = reported_usage(body);
        let budget = state
            .inference_budget
            .as_ref()
            .expect("governed memory observation");
        state
            .audit_sink
            .append(AuditEvent {
                timestamp_ms: audit_now_ms(),
                principal: state.sandbox_name.to_string(),
                action: format!("foundry:memory:service:{phase}:{}", self.id),
                payload_digest_hex: self.digest.clone(),
                verdict: phase.into(),
                labels: vec![
                    ("operation".into(), self.operation.clone()),
                    ("taskUid".into(), budget.identity.task_uid.clone()),
                    ("accountUid".into(), budget.binding.task.account.uid.clone()),
                    ("memoryBinding".into(), self.binding.clone()),
                    (
                        "accounting".into(),
                        "external_service_not_inference_settlement".into(),
                    ),
                    (
                        "usageStatus".into(),
                        if usage.is_null() {
                            "unreported"
                        } else {
                            "provider_reported_unsettled"
                        }
                        .into(),
                    ),
                    ("providerUsage".into(), usage.to_string()),
                    (
                        "httpStatus".into(),
                        status
                            .map(|s| s.as_u16().to_string())
                            .unwrap_or_else(|| "unknown".into()),
                    ),
                ],
            })
            .await?;
        Ok(())
    }

    pub async fn finish(&self, state: &AppState, status: Option<StatusCode>, body: &[u8]) {
        let phase = match status {
            Some(status) if status.is_success() => "response_received",
            Some(_) => "upstream_failure",
            None => "outcome_unknown",
        };
        if let Err(error) = self.append(state, phase, status, body).await {
            // Dispatch is already recorded. Preserve the upstream response so an
            // accepted update is not turned into a retryable local failure.
            tracing::error!(target: "inference.audit", request_digest = %self.digest,
                error = %error, "Memory outcome audit failed; dispatch remains unsettled");
        }
    }
}

fn reported_usage(body: &[u8]) -> Value {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return Value::Null;
    };
    let Some(usage) = value.get("usage").and_then(Value::as_object) else {
        return Value::Null;
    };
    let reported: serde_json::Map<String, Value> = [
        "input_tokens",
        "output_tokens",
        "prompt_tokens",
        "completion_tokens",
        "total_tokens",
    ]
    .into_iter()
    .filter_map(|key| {
        usage
            .get(key)
            .and_then(Value::as_u64)
            .map(|count| (key.into(), json!(count)))
    })
    .collect();
    if reported.is_empty() {
        Value::Null
    } else {
        Value::Object(reported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_service_usage_is_unknown_not_zero_and_never_copies_content() {
        for body in [
            b"{}".as_slice(),
            b"not-json",
            br#"{"usage":{"total_tokens":-1,"input_tokens":"secret"}}"#,
        ] {
            assert_eq!(reported_usage(body), Value::Null);
        }
        assert_eq!(reported_usage(br#"{"usage":{"total_tokens":17,"output_tokens":0,"content":"secret","scope":"private"}}"#),
            json!({"total_tokens":17,"output_tokens":0}));
    }
}
