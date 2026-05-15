// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
// ci:loc-ok: Slice-level module; decomposition tracked in §4.2 (see dev→main #320 promotion notes)

//! `EgressApproval` reconciler — Slice 5e of `crd-well-oiled-machine`.
//!
//! Drives an ephemeral grant from `Pending` → `Active` → `Expired`.
//!
//! ## State machine
//!
//! - **Pending** — the approval was admitted but is not yet in effect.
//!   Reasons: the named sibling `ClawSandbox` does not exist or is not
//!   yet `phase=Ready`, the router has not yet echoed the merged
//!   digest, or a defense-in-depth validation failed (bad TTL, bad
//!   reason bytes).
//! - **Active** — the per-approval file is in the sandbox's
//!   `clawsandbox-{sandbox}-egress-approvals` ConfigMap, the router
//!   has echoed the merged-allowlist digest under
//!   `PolicyKind::EgressApproval`, and `effective_at + ttl` has not
//!   yet elapsed. `effective_at` is stamped once and never moves.
//! - **Expired** — `now ≥ effective_at + ttl`. The CM key has been
//!   removed; the router watcher picks up the file drop on the next
//!   mtime tick and re-echoes the merged digest sans this approval.
//!   The finalizer is then removed.
//!
//! ## Authority model
//!
//! K8s RBAC. The `azureclaw:egress-approver` ClusterRole (shipped in
//! 5e.1) grants `create / get / list / delete` on this CRD; the k8s
//! audit log records who acted. The cryptographic attestation lane
//! (Slice 5e+, demand-gated) layers an optional ed25519 signature on
//! top — the spec is forward-compatible.
//!
//! ## §3 Ready ⇔ router echo
//!
//! Same shape as every other policy reconciler in this codebase
//! (ToolPolicy 1c, InferencePolicy 2a, ClawMemory 3a):
//!
//! 1. Controller compiles the canonical merged-allowlist bytes
//!    (`baseline ∪ all active sibling approvals' hosts`) and hashes
//!    them via [`crate::egress_approval_compile::merged_allowlist_digest`].
//! 2. The per-sandbox approvals ConfigMap is written via SSA, with a
//!    distinct field manager per approval so siblings cannot trample
//!    each other's keys.
//! 3. The router's `egress_allowlist_loader` reads the new file on
//!    the next mtime poll, replaces `Blocklist.allowlist`, and
//!    registers the merged digest under
//!    `PolicyKind::EgressApproval` on `/internal/policy-status`.
//! 4. This reconciler polls the sandbox's router admin endpoint and
//!    promotes `Pending → Active` only when the echo matches.
//!
//! ## Finalizer
//!
//! `azureclaw.azure.com/egress-approval-cleanup`. Ensures the CM key
//! is dropped before the CR is removed (so the router's next mtime
//! poll observes the file disappear and immediately drops the host
//! from the L7 allowlist).

use anyhow::Result;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use k8s_openapi::api::core::v1::ConfigMap;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::{
    Client, ResourceExt,
    api::{Api, ListParams, Patch, PatchParams},
    runtime::controller::{Action, Controller},
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

use crate::crd::{ClawSandbox, EndpointConfig};
use crate::egress_approval::{
    EgressApproval, EgressApprovalStatus,
    condition_reasons::{
        AWAITING_ROUTER_ECHO, BLOCKED_ON_SANDBOX, EXPIRED, REASON_INVALID, ROUTER_CONFIRMED,
        TTL_EXCEEDS_CEILING, TTL_INVALID,
    },
};
use crate::egress_approval_compile::{
    approval_file_key, approvals_configmap_name, compile_approval_file, merged_allowlist_digest,
};
use crate::status::conditions::{self, status as cond_status};
use crate::status::phase::{PHASE_ACTIVE, PHASE_EXPIRED, PHASE_PENDING, PHASE_SANDBOX_RUNNING};
use crate::status::router_confirmation::{RouterEnforcementState, decide_enforcement_state};
use crate::status::router_confirmation_io::poll_referencing_sandboxes;

const FIELD_MANAGER: &str = crate::field_managers::EGRESS_APPROVAL;
const FINALIZER: &str = "azureclaw.azure.com/egress-approval-cleanup";

/// Cluster-wide hard ceiling. The Helm-tunable `maxApprovalTtl` is
/// the soft, operator-visible default (24h); this is the absolute
/// upper bound the reconciler enforces regardless of what env was
/// passed in. Anything beyond a week is a misuse of the grant lane
/// (re-sign the baseline allowlist instead).
const HARD_TTL_CEILING_SECONDS: u64 = 7 * 24 * 3600;

/// Soft default if `EGRESS_APPROVAL_MAX_TTL_SECONDS` is unset.
const DEFAULT_TTL_CEILING_SECONDS: u64 = 24 * 3600;

const REQUEUE_AWAITING: Duration = Duration::from_secs(5);
const REQUEUE_PENDING_LONG: Duration = Duration::from_secs(30);
const REQUEUE_ACTIVE_MAX: Duration = Duration::from_secs(30);

/// Wire-protocol kind name the router uses on
/// `/internal/policy-status` for this CRD. Pinned in
/// `inference-router/src/policy_status.rs` as `PolicyKind::EgressApproval`.
const POLICY_KIND_WIRE: &str = "EgressApproval";

#[derive(Debug, thiserror::Error)]
enum ReconcileError {
    #[error("Kubernetes API error: {0}")]
    Kube(#[from] kube::Error),
    #[error("JSON serialization error: {0}")]
    SerdeJson(#[from] serde_json::Error),
}

impl ReconcileError {
    fn class(&self) -> &'static str {
        match self {
            ReconcileError::Kube(_) => "kube_api",
            ReconcileError::SerdeJson(_) => "serde",
        }
    }
}

struct Ctx {
    client: Client,
    http: reqwest::Client,
    ttl_ceiling_seconds: u64,
}

/// Result of validating `spec.ttl` + `spec.reason`. The reconciler
/// short-circuits to a terminal-pending stamp on the first
/// validation miss.
#[derive(Debug)]
enum ValidationError {
    TtlInvalid(String),
    TtlExceedsCeiling { requested: u64, ceiling: u64 },
    ReasonInvalid(&'static str),
}

impl ValidationError {
    fn reason(&self) -> &'static str {
        match self {
            ValidationError::TtlInvalid(_) => TTL_INVALID,
            ValidationError::TtlExceedsCeiling { .. } => TTL_EXCEEDS_CEILING,
            ValidationError::ReasonInvalid(_) => REASON_INVALID,
        }
    }

    fn message(&self) -> String {
        match self {
            ValidationError::TtlInvalid(s) => format!("spec.ttl invalid: {s}"),
            ValidationError::TtlExceedsCeiling {
                requested, ceiling, ..
            } => format!(
                "spec.ttl {requested}s exceeds cluster ceiling {ceiling}s; re-sign the baseline allowlist instead",
            ),
            ValidationError::ReasonInvalid(s) => format!("spec.reason invalid: {s}"),
        }
    }
}

/// Parse an ISO 8601 duration string of the forms `PT15M`, `PT4H`,
/// `P1D`, `PT1H30M`, etc. Returns total seconds (≥ 1).
///
/// We intentionally hand-roll the parser (no `iso8601` crate dep)
/// because the accepted grammar is tiny: positive integers attached
/// to `D`, `H`, `M`, `S` indicators after `P` and optional `T`. No
/// weeks (`W`), months, years; those are imprecise for TTL.
pub(crate) fn parse_iso8601_duration_secs(s: &str) -> Result<u64, String> {
    let raw = s.trim();
    if !raw.starts_with('P') {
        return Err("must start with 'P'".into());
    }
    let mut rest = &raw[1..];
    let mut seen_t = false;
    let mut secs: u64 = 0;
    let mut any_component = false;
    while !rest.is_empty() {
        if rest.starts_with('T') {
            if seen_t {
                return Err("duplicate 'T' separator".into());
            }
            seen_t = true;
            rest = &rest[1..];
            continue;
        }
        // Read integer prefix.
        let num_end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if num_end == 0 {
            return Err(format!("expected digit, got '{rest}'"));
        }
        let value: u64 = rest[..num_end]
            .parse()
            .map_err(|e| format!("integer overflow: {e}"))?;
        let designator = rest.as_bytes().get(num_end).copied().ok_or_else(|| {
            "trailing digits with no unit designator (expected D, H, M, or S)".to_string()
        })?;
        let unit_secs: u64 = match (seen_t, designator) {
            (false, b'D') => 24 * 3600,
            (true, b'H') => 3600,
            (true, b'M') => 60,
            (true, b'S') => 1,
            (false, b'H') | (false, b'M') | (false, b'S') => {
                return Err(format!(
                    "designator '{}' must follow 'T'",
                    designator as char
                ));
            }
            (true, b'D') => return Err("'D' is a date component, must precede 'T'".into()),
            (_, other) => return Err(format!("unknown designator '{}'", other as char)),
        };
        let contribution = value
            .checked_mul(unit_secs)
            .ok_or_else(|| "duration overflow".to_string())?;
        secs = secs
            .checked_add(contribution)
            .ok_or_else(|| "duration overflow".to_string())?;
        any_component = true;
        rest = &rest[num_end + 1..];
    }
    if !any_component {
        return Err("no components present".into());
    }
    if secs == 0 {
        return Err("duration must be > 0".into());
    }
    Ok(secs)
}

/// Reject ASCII control bytes (except whitespace tab/CR/LF) in
/// `spec.reason` — the field flows into structured audit-log lines
/// and `kubectl describe` output; embedded NUL or escape sequences
/// would either truncate downstream parsers or inject ANSI/terminal
/// control codes into operator terminals.
fn validate_reason(reason: &str) -> Result<(), ValidationError> {
    if reason.is_empty() {
        return Err(ValidationError::ReasonInvalid("empty"));
    }
    if reason.len() > 512 {
        return Err(ValidationError::ReasonInvalid("exceeds 512 bytes"));
    }
    for ch in reason.chars() {
        let c = ch as u32;
        let is_allowed_ws = matches!(c, 0x09 | 0x0A | 0x0D);
        let is_ctrl = c < 0x20 || c == 0x7F;
        if is_ctrl && !is_allowed_ws {
            return Err(ValidationError::ReasonInvalid(
                "contains ASCII control bytes (only tab/LF/CR permitted)",
            ));
        }
    }
    Ok(())
}

fn validate(approval: &EgressApproval, ttl_ceiling_seconds: u64) -> Result<u64, ValidationError> {
    validate_reason(&approval.spec.reason)?;
    let ttl_secs =
        parse_iso8601_duration_secs(&approval.spec.ttl).map_err(ValidationError::TtlInvalid)?;
    let effective_ceiling = ttl_ceiling_seconds.min(HARD_TTL_CEILING_SECONDS);
    if ttl_secs > effective_ceiling {
        return Err(ValidationError::TtlExceedsCeiling {
            requested: ttl_secs,
            ceiling: effective_ceiling,
        });
    }
    Ok(ttl_secs)
}

fn rfc3339_now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn rfc3339_at(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn parse_rfc3339(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Read the baseline allowlist endpoints from the controller-published
/// `clawsandbox-{sandbox}-egress-allowlist` ConfigMap (data key
/// `allowlist.json`). Missing CM / missing key / malformed JSON all
/// degrade gracefully to "empty baseline" — the merged digest just
/// reflects the approvals alone, and the router still drains its
/// blocklist correctly (see test
/// `baseline_no_binding_with_approvals_still_drains_blocklist` in
/// `inference-router/src/egress_allowlist_loader.rs`).
async fn read_baseline_endpoints(
    configmaps: &Api<ConfigMap>,
    sandbox: &str,
) -> Vec<EndpointConfig> {
    let cm_name = format!("clawsandbox-{sandbox}-egress-allowlist");
    let cm = match configmaps.get_opt(&cm_name).await {
        Ok(Some(cm)) => cm,
        _ => return Vec::new(),
    };
    let raw = match cm
        .data
        .as_ref()
        .and_then(|d| d.get(crate::egress_allowlist_compile::EGRESS_ALLOWLIST_FILENAME))
    {
        Some(s) => s.clone(),
        None => return Vec::new(),
    };
    let value: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let arr = match value.get("endpoints").and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return Vec::new(),
    };
    arr.iter()
        .filter_map(|entry| {
            let host = entry.get("host").and_then(|v| v.as_str())?.to_string();
            let port = entry.get("port").and_then(|v| v.as_u64()).map(|p| p as u16);
            if host.is_empty() {
                return None;
            }
            Some(EndpointConfig { host, port })
        })
        .collect()
}

/// List sibling `EgressApproval`s in the same namespace that point at
/// the same sandbox, excluding ourselves, anything mid-deletion, and
/// anything already `Expired`. Their `spec.hosts` are unioned into the
/// merged-allowlist digest.
async fn list_active_siblings(
    api: &Api<EgressApproval>,
    sandbox: &str,
    self_name: &str,
) -> Result<Vec<EgressApproval>, kube::Error> {
    let list = api.list(&ListParams::default()).await?;
    Ok(list
        .items
        .into_iter()
        .filter(|sibling| sibling.spec.sandbox == sandbox)
        .filter(|sibling| sibling.metadata.deletion_timestamp.is_none())
        .filter(|sibling| sibling.name_any() != self_name)
        .filter(|sibling| {
            sibling
                .status
                .as_ref()
                .and_then(|s| s.phase.as_deref())
                .map(|p| p != PHASE_EXPIRED)
                .unwrap_or(true)
        })
        .collect())
}

/// Compute the merged-allowlist digest for `baseline ∪ self.hosts ∪
/// sibling.hosts`. The router will echo this exact value once the
/// per-approval file lands in its mount dir.
fn compute_merged_digest(
    baseline: &[EndpointConfig],
    self_hosts: &[EndpointConfig],
    siblings: &[EgressApproval],
) -> String {
    let mut combined: Vec<EndpointConfig> = Vec::new();
    combined.extend_from_slice(self_hosts);
    for sibling in siblings {
        combined.extend(sibling.spec.hosts.iter().cloned());
    }
    merged_allowlist_digest(baseline, &combined)
}

/// SSA-patch the per-sandbox approvals ConfigMap to install (or
/// upsert) this approval's file. Field manager is scoped per
/// approval so siblings on the same CM cannot trample each other.
async fn ensure_approval_cm_key(
    configmaps: &Api<ConfigMap>,
    sandbox: &str,
    approval_name: &str,
    payload: &Value,
    field_manager: &str,
) -> Result<(), ReconcileError> {
    let cm_name = approvals_configmap_name(sandbox);
    let key = approval_file_key(approval_name);
    let body = serde_json::to_string(payload)?;
    let cm = json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {
            "name": cm_name,
            "labels": {
                "app.kubernetes.io/managed-by": "azureclaw-controller",
                "azureclaw.azure.com/sandbox": sandbox,
                "azureclaw.azure.com/artifact": "egress-approvals",
            },
        },
        "data": {
            &key: body,
        },
    });
    configmaps
        .patch(
            &cm_name,
            &PatchParams::apply(field_manager).force(),
            &Patch::Apply(&cm),
        )
        .await?;
    Ok(())
}

/// Drop this approval's key from the per-sandbox approvals CM.
/// Implemented as an SSA-apply with the per-approval field manager
/// but no `data` block — under SSA semantics, dropping the field from
/// our manager's owned set yields removal (because no other manager
/// owns it). Falls back to a JSON-merge null-patch for older API
/// servers that surface SSA edge cases.
async fn drop_approval_cm_key(
    configmaps: &Api<ConfigMap>,
    sandbox: &str,
    approval_name: &str,
    field_manager: &str,
) -> Result<(), ReconcileError> {
    let cm_name = approvals_configmap_name(sandbox);
    let key = approval_file_key(approval_name);

    let empty_apply = json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {
            "name": cm_name,
            "labels": {
                "app.kubernetes.io/managed-by": "azureclaw-controller",
                "azureclaw.azure.com/sandbox": sandbox,
                "azureclaw.azure.com/artifact": "egress-approvals",
            },
        },
        "data": {}
    });
    if let Err(e) = configmaps
        .patch(
            &cm_name,
            &PatchParams::apply(field_manager).force(),
            &Patch::Apply(&empty_apply),
        )
        .await
    {
        // CM may not exist yet — that's the equivalent of "already
        // dropped", treat as success.
        if matches!(&e, kube::Error::Api(ae) if ae.code == 404) {
            return Ok(());
        }
        return Err(e.into());
    }

    // Defense-in-depth: explicit null-merge so the key is removed
    // even on API servers that interpret an empty SSA `data` map as
    // "no-op". Failure here is non-fatal.
    let merge = json!({ "data": { &key: null } });
    let _ = configmaps
        .patch(&cm_name, &PatchParams::default(), &Patch::Merge(&merge))
        .await;
    Ok(())
}

async fn ensure_finalizer(
    api: &Api<EgressApproval>,
    approval: &EgressApproval,
    name: &str,
) -> Result<bool, ReconcileError> {
    let has = approval
        .metadata
        .finalizers
        .as_ref()
        .map(|f| f.iter().any(|s| s == FINALIZER))
        .unwrap_or(false);
    if has {
        return Ok(false);
    }
    let patch = json!({
        "apiVersion": "azureclaw.azure.com/v1alpha1",
        "kind": "EgressApproval",
        "metadata": { "finalizers": [FINALIZER] },
    });
    api.patch(
        name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(patch),
    )
    .await?;
    Ok(true)
}

async fn remove_finalizer(
    api: &Api<EgressApproval>,
    approval: &EgressApproval,
    name: &str,
) -> Result<(), ReconcileError> {
    let remaining: Vec<String> = approval
        .metadata
        .finalizers
        .as_ref()
        .map(|v| v.iter().filter(|f| *f != FINALIZER).cloned().collect())
        .unwrap_or_default();
    // SSA-applying just the metadata.finalizers field with our
    // manager so the API server treats removal correctly without
    // forcing whole-object ownership.
    let patch = json!({
        "apiVersion": "azureclaw.azure.com/v1alpha1",
        "kind": "EgressApproval",
        "metadata": { "finalizers": remaining },
    });
    api.patch(
        name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(patch),
    )
    .await?;
    Ok(())
}

/// Per-approval field manager — scoped so two approvals on the same
/// sandbox cannot fight over the merged CM's keys. The name still
/// embeds the base field manager prefix so audit tools that filter
/// on `azureclaw-controller/*` still pick it up.
fn approval_field_manager(approval_name: &str) -> String {
    format!("{FIELD_MANAGER}/{approval_name}")
}

/// Build the new status patch from the resolved enforcement state.
/// `prior_effective_at` is passed through unchanged on subsequent
/// reconciles — once stamped, it never moves (the TTL is measured
/// from this point).
#[allow(clippy::too_many_arguments)]
fn build_status_patch(
    prior_conditions: &[Condition],
    prior_effective_at: Option<String>,
    prior_usage_count: Option<i64>,
    observed_generation: Option<i64>,
    phase: &str,
    reason_value: &str,
    message: &str,
    merged_digest: Option<String>,
    host_count: i64,
    effective_at: Option<String>,
    expires_at: Option<String>,
    usage_count: Option<i64>,
) -> Value {
    let mut conds: Vec<Condition> = Vec::with_capacity(3);

    let (ready_status, ready_reason, ready_msg) = match phase {
        PHASE_ACTIVE => (cond_status::TRUE, ROUTER_CONFIRMED, message),
        PHASE_EXPIRED => (cond_status::FALSE, EXPIRED, message),
        _ => (cond_status::FALSE, reason_value, message),
    };
    conds.push(conditions::preserve_transition_time(
        conditions::find(prior_conditions, conditions::TYPE_READY),
        conditions::TYPE_READY,
        ready_status,
        ready_reason,
        ready_msg,
        observed_generation,
    ));

    let progressing_status = if phase == PHASE_ACTIVE || phase == PHASE_EXPIRED {
        cond_status::FALSE
    } else {
        cond_status::TRUE
    };
    conds.push(conditions::preserve_transition_time(
        conditions::find(prior_conditions, conditions::TYPE_PROGRESSING),
        conditions::TYPE_PROGRESSING,
        progressing_status,
        reason_value,
        message,
        observed_generation,
    ));

    let degraded_status = if matches!(
        reason_value,
        TTL_INVALID | TTL_EXCEEDS_CEILING | REASON_INVALID
    ) {
        cond_status::TRUE
    } else {
        cond_status::FALSE
    };
    conds.push(conditions::preserve_transition_time(
        conditions::find(prior_conditions, conditions::TYPE_DEGRADED),
        conditions::TYPE_DEGRADED,
        degraded_status,
        reason_value,
        message,
        observed_generation,
    ));

    let effective_final = effective_at.or(prior_effective_at);
    let usage_final = usage_count.or(prior_usage_count);

    let status = EgressApprovalStatus {
        phase: Some(phase.into()),
        observed_generation,
        effective_at: effective_final,
        expires_at,
        merged_digest,
        host_count: Some(host_count),
        usage_count: usage_final,
        conditions: Some(conds),
    };
    json!({
        "apiVersion": "azureclaw.azure.com/v1alpha1",
        "kind": "EgressApproval",
        "status": status,
    })
}

async fn write_status(
    api: &Api<EgressApproval>,
    name: &str,
    patch: Value,
) -> Result<(), ReconcileError> {
    api.patch_status(
        name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(patch),
    )
    .await?;
    Ok(())
}

async fn reconcile(approval: Arc<EgressApproval>, ctx: Arc<Ctx>) -> Result<Action, ReconcileError> {
    let name = approval.name_any();
    let ns = approval.namespace().unwrap_or_else(|| "default".into());
    let api: Api<EgressApproval> = Api::namespaced(ctx.client.clone(), &ns);
    // The approvals ConfigMap is consumed by the per-sandbox router which
    // mounts it from the sandbox *pod* namespace (`azureclaw-<sandbox>`),
    // NOT from the namespace where the EgressApproval CR lives (which is
    // typically the operator ns `azureclaw-system`). Bind the ConfigMap
    // Api to the sandbox pod namespace so the CM lands where the router
    // can read it.
    let sandbox_pod_ns = format!("azureclaw-{}", approval.spec.sandbox);
    let configmaps: Api<ConfigMap> = Api::namespaced(ctx.client.clone(), &sandbox_pod_ns);
    let sandboxes: Api<ClawSandbox> = Api::namespaced(ctx.client.clone(), &ns);

    if approval.metadata.deletion_timestamp.is_some() {
        return finalize(&api, &configmaps, &approval, &name).await;
    }
    if ensure_finalizer(&api, &approval, &name).await? {
        return Ok(Action::requeue(Duration::from_secs(1)));
    }

    let prior_status = approval.status.clone().unwrap_or_default();
    let prior_conditions = prior_status.conditions.clone().unwrap_or_default();
    let prior_effective_at = prior_status.effective_at.clone();
    let prior_usage_count = prior_status.usage_count;
    let observed_generation = approval.metadata.generation;
    let host_count = approval.merged_host_count() as i64;

    // ── Validate spec ────────────────────────────────────────────
    let ttl_secs = match validate(&approval, ctx.ttl_ceiling_seconds) {
        Ok(s) => s,
        Err(v) => {
            tracing::warn!(
                egress_approval = %name,
                ns = %ns,
                reason = v.reason(),
                "EgressApprovalSpecInvalid"
            );
            let patch = build_status_patch(
                &prior_conditions,
                prior_effective_at,
                prior_usage_count,
                observed_generation,
                PHASE_PENDING,
                v.reason(),
                &v.message(),
                None,
                host_count,
                None,
                None,
                None,
            );
            write_status(&api, &name, patch).await?;
            // Terminal-pending: requeue infrequently. Operator must
            // edit / recreate the CR to make progress.
            return Ok(Action::requeue(REQUEUE_PENDING_LONG));
        }
    };

    // ── Sibling sandbox check ────────────────────────────────────
    let sandbox = match sandboxes.get_opt(&approval.spec.sandbox).await? {
        Some(s) => s,
        None => {
            let msg = format!("ClawSandbox '{}' not found", approval.spec.sandbox);
            let patch = build_status_patch(
                &prior_conditions,
                prior_effective_at,
                prior_usage_count,
                observed_generation,
                PHASE_PENDING,
                BLOCKED_ON_SANDBOX,
                &msg,
                None,
                host_count,
                None,
                None,
                None,
            );
            write_status(&api, &name, patch).await?;
            return Ok(Action::requeue(REQUEUE_AWAITING));
        }
    };
    let sandbox_phase = sandbox
        .status
        .as_ref()
        .and_then(|s| s.phase.as_deref())
        .unwrap_or("");
    if sandbox_phase != PHASE_SANDBOX_RUNNING {
        let msg = format!(
            "ClawSandbox '{}' is not Running (phase='{}')",
            approval.spec.sandbox, sandbox_phase
        );
        let patch = build_status_patch(
            &prior_conditions,
            prior_effective_at,
            prior_usage_count,
            observed_generation,
            PHASE_PENDING,
            BLOCKED_ON_SANDBOX,
            &msg,
            None,
            host_count,
            None,
            None,
            None,
        );
        write_status(&api, &name, patch).await?;
        return Ok(Action::requeue(REQUEUE_AWAITING));
    }

    // ── Compute effective_at / expires_at (idempotent) ──────────
    let effective_at = prior_effective_at.clone().unwrap_or_else(rfc3339_now);
    let effective_at_dt = parse_rfc3339(&effective_at).unwrap_or_else(Utc::now);
    let expires_at_dt = effective_at_dt + chrono::Duration::seconds(ttl_secs as i64);
    let expires_at = rfc3339_at(expires_at_dt);

    // ── If already expired, finalize-expired right here ─────────
    let now = Utc::now();
    if now >= expires_at_dt {
        return finalize_expired(
            &api,
            &configmaps,
            &approval,
            &name,
            &prior_conditions,
            prior_usage_count,
            observed_generation,
            host_count,
            effective_at,
            expires_at,
        )
        .await;
    }

    // ── Compile + install per-approval file ─────────────────────
    let baseline = read_baseline_endpoints(&configmaps, &approval.spec.sandbox).await;
    let payload = compile_approval_file(
        &name,
        &approval.spec.sandbox,
        &approval.spec.hosts,
        &approval.spec.reason,
        approval.spec.ticket.as_deref(),
        &effective_at,
        &expires_at,
    );
    let per_approval_fm = approval_field_manager(&name);
    if let Err(e) = ensure_approval_cm_key(
        &configmaps,
        &approval.spec.sandbox,
        &name,
        &payload,
        &per_approval_fm,
    )
    .await
    {
        tracing::warn!(
            egress_approval = %name,
            error_class = e.class(),
            error = %e,
            "EgressApprovalCmWriteFailed"
        );
        // Fall through to status without confirmation — operator
        // sees Pending/AwaitingRouterEcho on transient CM write
        // failures and the next reconcile retries.
        let patch = build_status_patch(
            &prior_conditions,
            Some(effective_at.clone()),
            prior_usage_count,
            observed_generation,
            PHASE_PENDING,
            AWAITING_ROUTER_ECHO,
            &format!("approvals ConfigMap write failed: {e}"),
            None,
            host_count,
            Some(effective_at.clone()),
            Some(expires_at.clone()),
            None,
        );
        write_status(&api, &name, patch).await?;
        return Ok(Action::requeue(REQUEUE_AWAITING));
    }

    // ── Compute expected merged digest ──────────────────────────
    let siblings = list_active_siblings(&api, &approval.spec.sandbox, &name).await?;
    let expected_digest = compute_merged_digest(&baseline, &approval.spec.hosts, &siblings);

    // ── Poll router echo ────────────────────────────────────────
    let results = poll_referencing_sandboxes(
        &ctx.client,
        &ctx.http,
        std::slice::from_ref(&approval.spec.sandbox),
    )
    .await;
    let state = decide_enforcement_state(&expected_digest, POLICY_KIND_WIRE, &results);

    // The router's policy-status entry does not yet carry a
    // per-kind usage counter (Slice 5e ships the digest-echo loop;
    // usage_count is reserved for a later observability slice). We
    // pass the prior value through unchanged so the status field
    // stays monotonic.
    let usage_count = None;

    match state {
        RouterEnforcementState::Confirmed { total } => {
            let msg = format!(
                "router echoed merged digest ({total}/{total} sandboxes); expires at {expires_at}"
            );
            let patch = build_status_patch(
                &prior_conditions,
                Some(effective_at.clone()),
                prior_usage_count,
                observed_generation,
                PHASE_ACTIVE,
                ROUTER_CONFIRMED,
                &msg,
                Some(expected_digest),
                host_count,
                Some(effective_at.clone()),
                Some(expires_at.clone()),
                usage_count,
            );
            write_status(&api, &name, patch).await?;
            // Requeue at the earlier of expiry or 30s — gives us a
            // bounded staleness window for usage_count + a precise
            // wakeup at TTL elapse without spinning.
            let until_expiry = expires_at_dt - now;
            let until_expiry_secs = until_expiry.num_seconds().max(1) as u64;
            let requeue = Duration::from_secs(until_expiry_secs).min(REQUEUE_ACTIVE_MAX);
            Ok(Action::requeue(requeue))
        }
        RouterEnforcementState::Awaiting {
            total,
            matched,
            message,
        } => {
            let detail = format!(
                "awaiting router echo ({matched}/{total}); expected {expected_digest}; {message}"
            );
            let patch = build_status_patch(
                &prior_conditions,
                Some(effective_at.clone()),
                prior_usage_count,
                observed_generation,
                PHASE_PENDING,
                AWAITING_ROUTER_ECHO,
                &detail,
                Some(expected_digest),
                host_count,
                Some(effective_at.clone()),
                Some(expires_at.clone()),
                None,
            );
            write_status(&api, &name, patch).await?;
            Ok(Action::requeue(REQUEUE_AWAITING))
        }
        RouterEnforcementState::NoSandboxesReferencing | RouterEnforcementState::NotApplicable => {
            // Single-sandbox poll; this branch hits when the
            // input list to `poll_referencing_sandboxes` was empty
            // (NotApplicable) or aggregated to zero (shouldn't
            // happen here — defense-in-depth).
            let patch = build_status_patch(
                &prior_conditions,
                Some(effective_at.clone()),
                prior_usage_count,
                observed_generation,
                PHASE_PENDING,
                AWAITING_ROUTER_ECHO,
                "router not yet reachable",
                Some(expected_digest),
                host_count,
                Some(effective_at.clone()),
                Some(expires_at.clone()),
                None,
            );
            write_status(&api, &name, patch).await?;
            Ok(Action::requeue(REQUEUE_AWAITING))
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn finalize_expired(
    api: &Api<EgressApproval>,
    configmaps: &Api<ConfigMap>,
    approval: &EgressApproval,
    name: &str,
    prior_conditions: &[Condition],
    prior_usage_count: Option<i64>,
    observed_generation: Option<i64>,
    host_count: i64,
    effective_at: String,
    expires_at: String,
) -> Result<Action, ReconcileError> {
    let per_approval_fm = approval_field_manager(name);
    drop_approval_cm_key(configmaps, &approval.spec.sandbox, name, &per_approval_fm).await?;
    let patch = build_status_patch(
        prior_conditions,
        Some(effective_at.clone()),
        prior_usage_count,
        observed_generation,
        PHASE_EXPIRED,
        EXPIRED,
        &format!("approval expired at {expires_at}"),
        None,
        host_count,
        Some(effective_at),
        Some(expires_at),
        prior_usage_count,
    );
    write_status(api, name, patch).await?;
    // Remove finalizer so the CR can be garbage-collected by the
    // operator (or, if a deletion was issued mid-life, this lets it
    // disappear). The Expired stamp remains visible until then so
    // operators can audit grant history.
    remove_finalizer(api, approval, name).await?;
    Ok(Action::await_change())
}

async fn finalize(
    api: &Api<EgressApproval>,
    configmaps: &Api<ConfigMap>,
    approval: &EgressApproval,
    name: &str,
) -> Result<Action, ReconcileError> {
    let per_approval_fm = approval_field_manager(name);
    drop_approval_cm_key(configmaps, &approval.spec.sandbox, name, &per_approval_fm).await?;
    remove_finalizer(api, approval, name).await?;
    tracing::info!(egress_approval = %name, "EgressApprovalDeleted");
    Ok(Action::await_change())
}

fn error_policy(approval: Arc<EgressApproval>, error: &ReconcileError, _ctx: Arc<Ctx>) -> Action {
    crate::metrics::record_reconcile_error("EgressApproval", error.class());
    tracing::warn!(
        egress_approval = %approval.name_any(),
        error_class = error.class(),
        error = %error,
        "EgressApproval reconcile error — requeuing in ~15s (±20% jitter)"
    );
    Action::requeue(crate::backoff::requeue_secs_with_jitter(15))
}

fn ttl_ceiling_from_env() -> u64 {
    let raw = std::env::var("EGRESS_APPROVAL_MAX_TTL_SECONDS").ok();
    let parsed = raw
        .as_deref()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_TTL_CEILING_SECONDS);
    parsed.min(HARD_TTL_CEILING_SECONDS)
}

pub async fn run(client: Client) -> Result<()> {
    let approvals: Api<EgressApproval> = Api::all(client.clone());
    match approvals.list(&ListParams::default().limit(1)).await {
        Ok(_) => tracing::info!("EgressApproval CRD found — starting controller"),
        Err(e) => {
            tracing::warn!("EgressApproval CRD not installed — reconciler disabled: {e}");
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            return Ok(());
        }
    }
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| anyhow::anyhow!("reqwest build failed: {e}"))?;
    let ttl_ceiling_seconds = ttl_ceiling_from_env();
    tracing::info!(
        ttl_ceiling_seconds,
        "EgressApproval reconciler — TTL ceiling resolved from env"
    );
    let ctx = Arc::new(Ctx {
        client,
        http,
        ttl_ceiling_seconds,
    });
    Controller::new(approvals, kube::runtime::watcher::Config::default())
        .run(
            |x, ctx| async move {
                crate::metrics::observe_reconcile("EgressApproval", reconcile(x, ctx)).await
            },
            error_policy,
            ctx,
        )
        .for_each(|res| async move {
            match res {
                Ok(o) => tracing::debug!("EgressApproval reconciled {:?}", o),
                Err(e) => tracing::warn!("EgressApproval reconcile failed: {e:?}"),
            }
        })
        .await;
    Ok(())
}

// ───────────────────────── Tests ─────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes tests that mutate the `EGRESS_APPROVAL_MAX_TTL_SECONDS`
    /// process env var. `cargo test` runs tests in parallel by default;
    /// without this, tests race on the shared env and produce flaky
    /// `assert_eq!(v, HARD_TTL_CEILING_SECONDS)` failures (observed in
    /// PR #311 CI).
    static TTL_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    use crate::egress_approval::EgressApprovalSpec;

    fn mk_approval(name: &str, sandbox: &str, ttl: &str, reason: &str) -> EgressApproval {
        EgressApproval::new(
            name,
            EgressApprovalSpec {
                sandbox: sandbox.into(),
                hosts: vec![EndpointConfig {
                    host: "pypi.org".into(),
                    port: Some(443),
                }],
                reason: reason.into(),
                ticket: None,
                ttl: ttl.into(),
            },
        )
    }

    #[test]
    fn parse_iso8601_accepts_minutes_hours_days() {
        assert_eq!(parse_iso8601_duration_secs("PT15M").unwrap(), 900);
        assert_eq!(parse_iso8601_duration_secs("PT4H").unwrap(), 4 * 3600);
        assert_eq!(parse_iso8601_duration_secs("P1D").unwrap(), 86_400);
        assert_eq!(parse_iso8601_duration_secs("PT1H30M").unwrap(), 5400);
        assert_eq!(
            parse_iso8601_duration_secs("P1DT2H").unwrap(),
            86_400 + 7200
        );
        assert_eq!(parse_iso8601_duration_secs("PT30S").unwrap(), 30);
    }

    #[test]
    fn parse_iso8601_rejects_missing_p() {
        assert!(parse_iso8601_duration_secs("T15M").is_err());
    }

    #[test]
    fn parse_iso8601_rejects_empty_components() {
        assert!(parse_iso8601_duration_secs("P").is_err());
        assert!(parse_iso8601_duration_secs("PT").is_err());
    }

    #[test]
    fn parse_iso8601_rejects_zero() {
        assert!(parse_iso8601_duration_secs("PT0M").is_err());
    }

    #[test]
    fn parse_iso8601_rejects_weeks_months_years() {
        // 'W' is not in our accepted grammar.
        assert!(parse_iso8601_duration_secs("P1W").is_err());
        // 'Y' is not in our accepted grammar.
        assert!(parse_iso8601_duration_secs("P1Y").is_err());
    }

    #[test]
    fn parse_iso8601_rejects_hour_before_t() {
        // 'H' must follow 'T'.
        assert!(parse_iso8601_duration_secs("P1H").is_err());
    }

    #[test]
    fn parse_iso8601_rejects_day_after_t() {
        // 'D' is a date component; must precede 'T'.
        assert!(parse_iso8601_duration_secs("PT1D").is_err());
    }

    #[test]
    fn validate_reason_accepts_normal_text() {
        assert!(validate_reason("INC-1234 pypi access for incident").is_ok());
        assert!(validate_reason("multi\nline\nreason").is_ok());
        assert!(validate_reason("with\ttab").is_ok());
    }

    #[test]
    fn validate_reason_rejects_empty() {
        assert!(matches!(
            validate_reason(""),
            Err(ValidationError::ReasonInvalid(_))
        ));
    }

    #[test]
    fn validate_reason_rejects_oversize() {
        let huge: String = "a".repeat(513);
        assert!(matches!(
            validate_reason(&huge),
            Err(ValidationError::ReasonInvalid(_))
        ));
    }

    #[test]
    fn validate_reason_rejects_control_bytes() {
        assert!(matches!(
            validate_reason("evil\x07bell"),
            Err(ValidationError::ReasonInvalid(_))
        ));
        assert!(matches!(
            validate_reason("nul\x00byte"),
            Err(ValidationError::ReasonInvalid(_))
        ));
        // ANSI escape sequence intro — exactly what we don't want
        // dropped into operator terminals via `kubectl describe`.
        assert!(matches!(
            validate_reason("escape\x1b[31mred"),
            Err(ValidationError::ReasonInvalid(_))
        ));
    }

    #[test]
    fn validate_rejects_ttl_exceeding_ceiling() {
        let appr = mk_approval("a", "demo", "P2D", "incident");
        let err = validate(&appr, DEFAULT_TTL_CEILING_SECONDS).unwrap_err();
        assert!(matches!(err, ValidationError::TtlExceedsCeiling { .. }));
        assert_eq!(err.reason(), TTL_EXCEEDS_CEILING);
    }

    #[test]
    fn validate_caps_to_hard_ceiling_when_env_too_large() {
        // Operator misconfigured env to 30 days — hard ceiling clamps
        // to 7 days, so a 10-day TTL is rejected.
        let appr = mk_approval("a", "demo", "P10D", "incident");
        let err = validate(&appr, 30 * 24 * 3600).unwrap_err();
        match err {
            ValidationError::TtlExceedsCeiling { ceiling, .. } => {
                assert_eq!(ceiling, HARD_TTL_CEILING_SECONDS);
            }
            other => panic!(
                "expected TtlExceedsCeiling, got {other:?}",
                other = other.reason()
            ),
        }
    }

    #[test]
    fn validate_accepts_within_ceiling() {
        let appr = mk_approval("a", "demo", "PT15M", "incident");
        assert_eq!(validate(&appr, DEFAULT_TTL_CEILING_SECONDS).unwrap(), 900);
    }

    #[test]
    fn approval_field_manager_is_scoped_per_approval() {
        let a = approval_field_manager("inc-1");
        let b = approval_field_manager("inc-2");
        assert_ne!(a, b);
        assert!(a.starts_with(FIELD_MANAGER));
        assert!(b.starts_with(FIELD_MANAGER));
    }

    #[test]
    fn ttl_ceiling_from_env_defaults_when_unset() {
        // Don't actually mutate env in unit tests — just exercise the
        // helper against the default path. The 'env not set' case is
        // the most-common operator deployment.
        let _guard = TTL_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::remove_var("EGRESS_APPROVAL_MAX_TTL_SECONDS");
        }
        assert_eq!(ttl_ceiling_from_env(), DEFAULT_TTL_CEILING_SECONDS);
    }

    #[test]
    fn ttl_ceiling_from_env_caps_at_hard_ceiling() {
        // Env override above the hard ceiling clamps down.
        let _guard = TTL_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var("EGRESS_APPROVAL_MAX_TTL_SECONDS", "9999999");
        }
        let v = ttl_ceiling_from_env();
        unsafe {
            std::env::remove_var("EGRESS_APPROVAL_MAX_TTL_SECONDS");
        }
        assert_eq!(v, HARD_TTL_CEILING_SECONDS);
    }

    #[test]
    fn ttl_ceiling_from_env_ignores_zero_or_invalid() {
        let _guard = TTL_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var("EGRESS_APPROVAL_MAX_TTL_SECONDS", "0");
        }
        assert_eq!(ttl_ceiling_from_env(), DEFAULT_TTL_CEILING_SECONDS);
        unsafe {
            std::env::set_var("EGRESS_APPROVAL_MAX_TTL_SECONDS", "garbage");
        }
        assert_eq!(ttl_ceiling_from_env(), DEFAULT_TTL_CEILING_SECONDS);
        unsafe {
            std::env::remove_var("EGRESS_APPROVAL_MAX_TTL_SECONDS");
        }
    }

    #[test]
    fn build_status_patch_active_sets_ready_true_progressing_false() {
        let v = build_status_patch(
            &[],
            None,
            None,
            Some(1),
            PHASE_ACTIVE,
            ROUTER_CONFIRMED,
            "router echoed",
            Some("sha256:deadbeef".into()),
            2,
            Some("2026-05-15T10:00:00Z".into()),
            Some("2026-05-15T10:15:00Z".into()),
            Some(5),
        );
        let status = v.get("status").unwrap();
        assert_eq!(status.get("phase").and_then(|x| x.as_str()), Some("Active"));
        assert_eq!(status.get("hostCount").and_then(|x| x.as_i64()), Some(2));
        assert_eq!(status.get("usageCount").and_then(|x| x.as_i64()), Some(5));
        let conds = status.get("conditions").and_then(|x| x.as_array()).unwrap();
        assert_eq!(conds.len(), 3);
        let ready = conds
            .iter()
            .find(|c| c.get("type").and_then(|x| x.as_str()) == Some("Ready"))
            .unwrap();
        assert_eq!(ready.get("status").and_then(|x| x.as_str()), Some("True"));
        assert_eq!(
            ready.get("reason").and_then(|x| x.as_str()),
            Some(ROUTER_CONFIRMED),
        );
        let prog = conds
            .iter()
            .find(|c| c.get("type").and_then(|x| x.as_str()) == Some("Progressing"))
            .unwrap();
        assert_eq!(prog.get("status").and_then(|x| x.as_str()), Some("False"));
        let degraded = conds
            .iter()
            .find(|c| c.get("type").and_then(|x| x.as_str()) == Some("Degraded"))
            .unwrap();
        assert_eq!(
            degraded.get("status").and_then(|x| x.as_str()),
            Some("False")
        );
    }

    #[test]
    fn build_status_patch_pending_blocked_on_sandbox() {
        let v = build_status_patch(
            &[],
            None,
            None,
            Some(1),
            PHASE_PENDING,
            BLOCKED_ON_SANDBOX,
            "not ready",
            None,
            1,
            None,
            None,
            None,
        );
        let status = v.get("status").unwrap();
        assert_eq!(
            status.get("phase").and_then(|x| x.as_str()),
            Some("Pending")
        );
        let conds = status.get("conditions").and_then(|x| x.as_array()).unwrap();
        let ready = conds
            .iter()
            .find(|c| c.get("type").and_then(|x| x.as_str()) == Some("Ready"))
            .unwrap();
        assert_eq!(ready.get("status").and_then(|x| x.as_str()), Some("False"));
        assert_eq!(
            ready.get("reason").and_then(|x| x.as_str()),
            Some(BLOCKED_ON_SANDBOX),
        );
        let prog = conds
            .iter()
            .find(|c| c.get("type").and_then(|x| x.as_str()) == Some("Progressing"))
            .unwrap();
        assert_eq!(prog.get("status").and_then(|x| x.as_str()), Some("True"));
    }

    #[test]
    fn build_status_patch_terminal_invalid_sets_degraded_true() {
        let v = build_status_patch(
            &[],
            None,
            None,
            Some(1),
            PHASE_PENDING,
            TTL_INVALID,
            "bad ttl",
            None,
            1,
            None,
            None,
            None,
        );
        let conds = v
            .get("status")
            .and_then(|s| s.get("conditions"))
            .and_then(|c| c.as_array())
            .unwrap();
        let degraded = conds
            .iter()
            .find(|c| c.get("type").and_then(|x| x.as_str()) == Some("Degraded"))
            .unwrap();
        assert_eq!(
            degraded.get("status").and_then(|x| x.as_str()),
            Some("True")
        );
        assert_eq!(
            degraded.get("reason").and_then(|x| x.as_str()),
            Some(TTL_INVALID),
        );
    }

    #[test]
    fn build_status_patch_expired_preserves_effective_at_and_usage() {
        // Idempotency contract: effective_at stamped once, never moves.
        // After expiry, usage_count is preserved (historical).
        let v = build_status_patch(
            &[],
            Some("2026-05-15T10:00:00Z".into()), // prior_effective_at
            Some(42),                            // prior_usage_count
            Some(3),
            PHASE_EXPIRED,
            EXPIRED,
            "expired",
            None,
            1,
            Some("2026-05-15T10:00:00Z".into()),
            Some("2026-05-15T10:15:00Z".into()),
            Some(42),
        );
        let status = v.get("status").unwrap();
        assert_eq!(
            status.get("effectiveAt").and_then(|x| x.as_str()),
            Some("2026-05-15T10:00:00Z"),
        );
        assert_eq!(status.get("usageCount").and_then(|x| x.as_i64()), Some(42));
    }

    #[test]
    fn build_status_patch_preserves_prior_effective_at_when_none_passed() {
        // Subsequent reconciles pass `None` for the new effective_at;
        // the helper folds the prior value through so the timestamp
        // never moves (TTL is measured from this point).
        let v = build_status_patch(
            &[],
            Some("2026-05-15T10:00:00Z".into()),
            None,
            Some(2),
            PHASE_PENDING,
            BLOCKED_ON_SANDBOX,
            "still pending",
            None,
            1,
            None,
            None,
            None,
        );
        assert_eq!(
            v.get("status")
                .and_then(|s| s.get("effectiveAt"))
                .and_then(|x| x.as_str()),
            Some("2026-05-15T10:00:00Z"),
        );
    }

    #[test]
    fn parse_rfc3339_round_trip() {
        let now = rfc3339_now();
        let parsed = parse_rfc3339(&now).expect("round-trips");
        let again = rfc3339_at(parsed);
        assert_eq!(now, again);
    }

    #[test]
    fn compute_merged_digest_byte_identical_to_compile_module() {
        // Independently call merged_allowlist_digest with the same
        // inputs the reconciler would build internally; the values
        // must match. This pins the cross-binary parity contract
        // even when refactors split the path.
        let baseline = vec![EndpointConfig {
            host: "EXAMPLE.com".into(),
            port: Some(443),
        }];
        let self_hosts = vec![EndpointConfig {
            host: "pypi.org".into(),
            port: None,
        }];
        let siblings = vec![mk_approval("sib", "demo", "PT15M", "ok")];
        let direct = {
            let mut combined: Vec<EndpointConfig> = Vec::new();
            combined.extend(self_hosts.iter().cloned());
            for s in &siblings {
                combined.extend(s.spec.hosts.iter().cloned());
            }
            merged_allowlist_digest(&baseline, &combined)
        };
        let via = compute_merged_digest(&baseline, &self_hosts, &siblings);
        assert_eq!(via, direct);
    }

    #[test]
    fn finalizer_constant_is_dns_subdomain() {
        assert!(FINALIZER.contains('/'));
        let (domain, _) = FINALIZER.split_once('/').unwrap();
        assert_eq!(domain, "azureclaw.azure.com");
    }

    #[test]
    fn field_manager_distinct_from_other_reconcilers() {
        assert_eq!(FIELD_MANAGER, "azureclaw-controller/egressapproval");
        assert_ne!(FIELD_MANAGER, "azureclaw-controller/claweval");
        assert_ne!(FIELD_MANAGER, "azureclaw-controller/clawmemory");
        assert_ne!(FIELD_MANAGER, "azureclaw-controller/toolpolicy");
    }

    #[test]
    fn policy_kind_wire_matches_router() {
        // PolicyKind name on the wire is "EgressApproval" exactly —
        // pinned in inference-router/src/policy_status.rs.
        assert_eq!(POLICY_KIND_WIRE, "EgressApproval");
    }
}
