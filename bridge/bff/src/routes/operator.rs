// Copyright (c) Pal Lakatos-Toth.
// kars Bridge BFF — Operator Console API.
//
// The operator surface reads the same CRDs as the user Workspace but projects
// them into resource/policy/audit language. Every field here is read from live
// cluster state via the dynamic API (no typed consumer per CRD) and projected
// into a stable browser DTO. Absent CRDs surface as empty lists, never errors —
// the honesty grammar (empty vs not-wired) lives in the web layer.

mod additional_providers;
mod audit;
mod diagnostics;
mod evals;
mod local_inference;
mod policies;
mod providers;
mod sandboxes;
mod skills_profiles;

pub use additional_providers::*;
pub use audit::*;
pub use diagnostics::*;
pub use evals::*;
pub use local_inference::*;
pub use policies::*;
pub use providers::*;
pub use sandboxes::*;
pub use skills_profiles::*;

use axum::Json;
use axum::extract::{Extension, State};
use kube::core::DynamicObject;
use serde_json::Value;

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

fn require_cluster(state: &AppState) -> AppResult<&crate::kars::cluster::Cluster> {
    state.cluster().ok_or(AppError::ClusterUnavailable)
}

fn upstream(e: kube::Error) -> AppError {
    AppError::Upstream(e.to_string())
}

// ─── helpers over DynamicObject ──────────────────────────────────────────────

fn name_of(o: &DynamicObject) -> String {
    o.metadata.name.clone().unwrap_or_default()
}
fn ns_of(o: &DynamicObject) -> String {
    o.metadata.namespace.clone().unwrap_or_default()
}
fn created_of(o: &DynamicObject) -> Option<String> {
    o.metadata
        .creation_timestamp
        .as_ref()
        .map(|t| t.0.to_rfc3339())
}
/// Whether this sandbox is owned by a KarsTask — true for every mission/team-
/// run sandbox, false for a standing sandbox with no task behind it (e.g. the
/// Bridge's own orchestrator sandbox). Such a sandbox never gets a
/// mission-output ConfigMap, so it must be excluded from the "executing" test
/// below (it would otherwise look permanently "not yet delivered").
fn has_task_owner(o: &DynamicObject) -> bool {
    o.metadata
        .owner_references
        .as_ref()
        .is_some_and(|refs| refs.iter().any(|r| r.kind == "KarsTask"))
}
fn spec(o: &DynamicObject) -> &Value {
    o.data.get("spec").unwrap_or(&Value::Null)
}
fn status(o: &DynamicObject) -> &Value {
    o.data.get("status").unwrap_or(&Value::Null)
}
fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|x| x.to_string())
}
fn label(o: &DynamicObject, key: &str) -> Option<String> {
    o.metadata.labels.as_ref().and_then(|l| l.get(key).cloned())
}
fn annotation(o: &DynamicObject, key: &str) -> Option<String> {
    o.metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(key).cloned())
}

// ─── Credentials (secure repo/system access for agents) ──────────────────────

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialRequest {
    /// The agent/team this credential is for (becomes `<target>-credentials`).
    pub target: String,
    pub kind: String,
    pub namespace: String,
    #[serde(default)]
    pub target_uid: Option<String>,
    /// The env var name the agent reads (e.g. GITHUB_TOKEN, BRAVE_API_KEY).
    pub key: String,
    /// The secret value. Stored only in the K8s Secret; never read back.
    pub value: String,
    #[serde(default)]
    pub review: Option<String>,
}

/// A DNS-1123 label (lowercase alphanumeric + hyphens, must start/end
/// alphanumeric, ≤63 chars) — the constraint on the `kars-<target>` namespace
/// derived below, so an invalid target is rejected before it reaches the API
/// server as an opaque 422.
pub(super) fn is_dns1123_label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

/// A POSIX-ish environment variable name: letters/digits/underscore, not
/// starting with a digit. Agents read the credential under this name.
pub(super) fn is_env_key(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub(super) fn credential_write_error(error: kube::Error) -> AppError {
    match error {
        kube::Error::Api(status) if status.code == 409 => AppError::Conflict(
            "Credential authority changed or a source already exists. Refresh credential metadata and review before resubmitting; a source write may already be stored. No automatic retry or rollback was attempted.".into(),
        ),
        kube::Error::Api(status) => {
            AppError::Upstream(format!("Credential write: Kubernetes status {}", status.code))
        }
        _ => AppError::Upstream("Credential write transport or serialization failed".into()),
    }
}

/// `POST /api/operator/credentials` — write a governed workspace source and
/// bind its actual UID to the reviewed target. Values are write-only.
/// A binding conflict may follow a committed source write; report 409 without
/// retrying the transaction or deleting a source whose delivery is uncertain.
pub async fn put_credential(
    State(state): State<AppState>,
    principal: Option<Extension<Principal>>,
    Json(req): Json<CredentialRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let target = req.target.trim();
    let key = req.key.trim();
    if target.is_empty() || key.is_empty() || req.value.is_empty() {
        return Err(AppError::BadRequest(
            "target, key and value are required".into(),
        ));
    }
    // Validate client-supplied names client-side of the API server, so the
    // failure is an actionable 400 rather than an opaque Kubernetes 422.
    if !is_dns1123_label(target) {
        return Err(AppError::BadRequest(
            "target must be a DNS-1123 label (lowercase letters, digits, hyphens; not starting/ending with a hyphen; ≤63 chars)".into(),
        ));
    }
    if !is_env_key(key) {
        return Err(AppError::BadRequest(
            "key must be a valid environment variable name (letters, digits, underscore; not starting with a digit)".into(),
        ));
    }
    if !is_dns1123_label(&req.namespace)
        || !["KarsSandbox", "KarsTask", "KarsTeam"].contains(&req.kind.as_str())
    {
        return Err(AppError::BadRequest("An explicit workspace namespace and KarsSandbox/KarsTask/KarsTeam target kind are required".into()));
    }
    if req.review.is_some() {
        let principal = principal.ok_or_else(|| {
            AppError::Forbidden(
                "A verified operator is required for reviewed credential writes".into(),
            )
        })?;
        return super::credential_review::write(&state, &principal.0, req).await;
    }
    Ok(Json(
        cluster
            .write_agent_credentials(
                &req.namespace,
                &req.kind,
                target,
                req.target_uid.as_deref(),
                std::collections::BTreeMap::from([(key.to_string(), req.value)]),
                Vec::new(),
            )
            .await
            .map_err(credential_write_error)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::{is_dns1123_label, is_env_key};

    #[test]
    fn dns1123_label_rules() {
        assert!(is_dns1123_label("repo-watch"));
        assert!(is_dns1123_label("a"));
        assert!(is_dns1123_label("team1"));
        assert!(!is_dns1123_label("")); // empty
        assert!(!is_dns1123_label("-lead")); // leading hyphen
        assert!(!is_dns1123_label("lead-")); // trailing hyphen
        assert!(!is_dns1123_label("Repo")); // uppercase
        assert!(!is_dns1123_label("a_b")); // underscore
        assert!(!is_dns1123_label(&"x".repeat(64))); // too long
    }

    #[test]
    fn env_key_rules() {
        assert!(is_env_key("GITHUB_TOKEN"));
        assert!(is_env_key("_x"));
        assert!(is_env_key("BRAVE_API_KEY"));
        assert!(!is_env_key("")); // empty
        assert!(!is_env_key("1TOKEN")); // leading digit
        assert!(!is_env_key("MY-KEY")); // hyphen
        assert!(!is_env_key("MY KEY")); // space
    }
}
