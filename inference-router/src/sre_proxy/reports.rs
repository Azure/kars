// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{Proxy, error};
use crate::sre_report::{self, Identity, Input, Report};
use agentmesh_mcp::redactor::CredentialRedactor;
use axum::{
    Json,
    body::Bytes,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use futures::StreamExt;
use serde_json::{Value, json};

async fn bounded_json(response: reqwest::Response) -> Result<Value, &'static str> {
    if !response.status().is_success() {
        return Err("Report storage request failed");
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "Report storage response incomplete")?;
        if bytes.len() + chunk.len() > 128 * 1024 {
            return Err("Report storage response oversized");
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "Report storage response invalid")
}

fn sanitize(input: &mut Input) -> Result<(), &'static str> {
    if input.text.len() > sre_report::MAX_TEXT {
        return Err("Report text exceeds limit");
    }
    // Strip terminal escapes before credential detection; preserve readable line breaks.
    let ansi = regex::Regex::new(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1b\\))")
        .expect("static ANSI pattern");
    let plain = ansi.replace_all(&input.text, "");
    let plain: String = plain
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect();
    input.text = CredentialRedactor::new().redact(&plain).sanitized;
    if input.text.len() > sre_report::MAX_TEXT {
        let mut end = sre_report::MAX_TEXT;
        while !input.text.is_char_boundary(end) {
            end -= 1;
        }
        input.text.truncate(end);
        input.truncated = true;
    }
    input.validate()
}

pub(super) async fn publish(
    proxy: &Proxy,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if method != Method::POST
        || uri.query().is_some()
        || uri.authority().is_some()
        || headers.contains_key("upgrade")
    {
        return error(
            StatusCode::FORBIDDEN,
            "Only report publication is permitted",
        );
    }
    if headers.get("content-type").and_then(|v| v.to_str().ok()) != Some("application/json") {
        return error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Report requires application/json",
        );
    }
    let mut input: Input = match serde_json::from_slice(&body) {
        Ok(input) => input,
        Err(_) => return error(StatusCode::BAD_REQUEST, "Invalid report JSON"),
    };
    if let Err(reason) = sanitize(&mut input) {
        return error(StatusCode::BAD_REQUEST, reason);
    }
    let now = chrono::Utc::now().timestamp_millis() as u64;
    if input.finished_at_ms > now.saturating_add(30_000)
        || input.finished_at_ms.saturating_add(300_000) < now
    {
        return error(
            StatusCode::BAD_REQUEST,
            "Report completion is outside the publication window",
        );
    }
    let Ok(_permit) = proxy.capacity.try_acquire() else {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "SRE proxy capacity is exhausted",
        );
    };
    match persist(proxy, input, now).await {
        Ok(report) => Json(json!({"persisted":true,"sweep_id":report.report.sweep_id,
            "digest":report.digest,"accepted_at_ms":report.accepted_at_ms,
            "enforcement_changed":false}))
        .into_response(),
        Err((status, reason)) => error(status, reason),
    }
}

async fn persist(
    proxy: &Proxy,
    input: Input,
    now: u64,
) -> Result<Report, (StatusCode, &'static str)> {
    let unavailable = || {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "SRE report authority or storage unavailable",
        )
    };
    let conflict = || {
        (
            StatusCode::CONFLICT,
            "SRE report target or publication changed",
        )
    };
    let config = &proxy.backend.config;
    let identity = Identity {
        registration_uid: config.registration_uid.clone(),
        source_namespace: config.source.namespace.clone(),
        source_uid: config.source.uid.clone(),
        namespace_uid: config.namespace_uid.clone(),
    };
    let path = format!(
        "/api/v1/namespaces/{}/configmaps/{}",
        sre_report::NAMESPACE,
        sre_report::NAME
    );
    let response = proxy
        .backend
        .forward(Method::GET, &path, None, false)
        .await
        .map_err(|_| unavailable())?;
    let mut target = bounded_json(response).await.map_err(|_| unavailable())?;
    if !identity.matches_target(&target) {
        return Err(conflict());
    }
    if target["data"].is_null() {
        target["data"] = json!({});
    }
    let data = target["data"].as_object().ok_or_else(conflict)?;
    if data.keys().any(|key| key != sre_report::KEY) {
        return Err(conflict());
    }
    if let Some(stored) = data.get(sre_report::KEY) {
        let prior =
            Report::decode(stored.as_str().ok_or_else(conflict)?).map_err(|_| conflict())?;
        if prior.identity != identity {
            return Err(conflict());
        }
        if prior.report.sweep_id == input.sweep_id {
            if prior.report != input || prior.privacy_epoch != config.privacy_epoch {
                return Err(conflict());
            }
            proxy.backend.authorize().await.map_err(|_| unavailable())?;
            return Ok(prior);
        }
        if prior.report.finished_at_ms >= input.finished_at_ms {
            return Err(conflict());
        }
    }
    let report = Report {
        version: 1,
        identity,
        privacy_epoch: config.privacy_epoch.clone(),
        accepted_at_ms: now,
        digest: input.digest(),
        report: input,
    };
    let encoded = serde_json::to_string(&report).map_err(|_| unavailable())?;
    if encoded.len() > sre_report::MAX_RECORD {
        return Err((StatusCode::BAD_REQUEST, "Report exceeds storage limit"));
    }
    target["data"] = json!({(sre_report::KEY):encoded});
    // forward rechecks live registration/source/privacy immediately before the CAS write.
    let response = proxy
        .backend
        .forward(Method::PUT, &path, Some(target.clone()), false)
        .await
        .map_err(|_| unavailable())?;
    if response.status() == StatusCode::CONFLICT {
        return Err(conflict());
    }
    let written = bounded_json(response).await.map_err(|_| unavailable())?;
    if !report.identity.matches_target(&written)
        || written["metadata"]["uid"] != target["metadata"]["uid"]
        || written["data"] != target["data"]
    {
        return Err(conflict());
    }
    proxy.backend.authorize().await.map_err(|_| unavailable())?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitization_reuses_credential_redactor_and_preserves_unknown_coverage() {
        let mut input = Input {
            sweep_id: "a".repeat(32),
            started_at_ms: 1,
            finished_at_ms: 2,
            turn_status: "blocked".into(),
            text: "\u{1b}[31mRead failed\u{1b}[0m\ncoverage unknown".into(),
            truncated: false,
            interval_seconds: 900,
            timeout_seconds: 180,
        };
        sanitize(&mut input).unwrap();
        assert_eq!(input.text, "Read failed\ncoverage unknown");
        assert_eq!(input.turn_status, "blocked");
        let credential = format!("ghp_{}", "A".repeat(36));
        input.text = format!("coverage unknown; credential {credential}");
        let expected = CredentialRedactor::new().redact(&input.text);
        assert!(!expected.detected.is_empty());
        sanitize(&mut input).unwrap();
        assert_eq!(input.text, expected.sanitized);
        assert!(!input.text.contains(&credential));
        input.text = "x".repeat(sre_report::MAX_TEXT + 1);
        assert!(sanitize(&mut input).is_err());
    }
}
