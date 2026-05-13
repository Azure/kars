// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `InferencePolicy` compiled-profile loader (Slice 2a).
//!
//! Reads a single JSON file from a mount directory and registers the
//! result with the `PolicyStatusRegistry` so the controller-side
//! `inference_policy_reconciler` can close the principles.md §3
//! "Ready ⇔ router echo" loop. The loaded policy is also cached in an
//! `Arc<RwLock<Option<LoadedInferencePolicy>>>` so the inference
//! pipeline can look up the active `perRequestTokens` cap without
//! re-reading the file on every request.
//!
//! ## Slice 2a scope
//!
//! Today only **`tokenBudget.perRequestTokens`** is consumed by the
//! router. The compiled JSON shape (produced by the controller's
//! `inference_policy_compile::compile_to_profile`) carries the full
//! spec — `contentSafety`, `modelPreference`, `appliesTo` — but those
//! fields stay parked in the `LoadedInferencePolicy.raw` JSON until
//! the corresponding sub-slices wire enforcement.
//!
//! Loading two policies at once is **not supported** in 2a: each
//! router process serves exactly one sandbox, and the sandbox
//! references at most one `InferencePolicy` via
//! `ClawSandbox.spec.inferenceRef`. The loader picks the first
//! `*.json` file in the mount directory (sorted) so behaviour is
//! deterministic if multiple files appear during a transitional
//! mirror update.
//!
//! ## Digest contract
//!
//! The digest registered with `PolicyStatusRegistry` is computed over
//! length-prefixed canonical bytes `u64-BE(name.len()) || name ||
//! u64-BE(body.len()) || body`, identical to the
//! `Governance::load_policies_from_dir` aggregation for AGT profiles.
//! The controller's
//! `inference_policy_compile::inference_policy_digest` computes the
//! same canonical-bytes layout for the single file
//! `inference-policy.json`, so a byte-identical load yields a
//! byte-identical digest. **Do not change this layout without a
//! coordinated controller-side update.**

use crate::policy_status::{PolicyKind, PolicyStatusRegistry};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Canonical filename the controller writes to the
/// `inferencepolicy-<name>-profile` ConfigMap. Kept in lockstep with
/// `inference_policy_compile::INFERENCE_POLICY_FILENAME` on the
/// controller side — the byte layout is part of the wire contract.
pub const INFERENCE_POLICY_FILENAME: &str = "inference-policy.json";

/// Shared handle to the currently loaded `InferencePolicy`, or `None`
/// if no policy was loaded (file missing / parse error / empty mount).
/// Wired into `routes::AppState` so handlers can read the active
/// `perRequestTokens` without re-reading the file.
pub type LoadedInferencePolicyHandle = Arc<RwLock<Option<LoadedInferencePolicy>>>;

/// Parsed `InferencePolicy` profile cached in memory. The `raw` field
/// preserves the JSON for later sub-slices that consume more axes
/// (`contentSafety`, `modelPreference`); 2a only consumes
/// `per_request_tokens`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedInferencePolicy {
    /// `sha256:<hex>` digest the router will echo back to the
    /// controller through `GET /internal/policy-status` once
    /// `record_success` is called below.
    pub digest: String,

    /// Filesystem path the bytes came from. Surfaced in
    /// `PolicyStatusEntry.source_path`.
    pub source_path: String,

    /// `spec.tokenBudget.perRequestTokens` — enforced in Slice 2a
    /// via [`crate::routes::chat_completions::decide_per_request_gate`].
    /// `None` means the policy did not set a per-request cap; the
    /// router falls back to no enforcement on that axis.
    pub per_request_tokens: Option<u64>,

    /// `spec.tokenBudget.dailyTokens` — enforced in Slice 2b by the
    /// UTC-calendar `TokenBudgetTracker`. `None` falls back to the
    /// env-driven `TOKEN_BUDGET_DAILY` safety-net cap (or unlimited
    /// when that env is also unset).
    pub daily_tokens: Option<u64>,

    /// `spec.tokenBudget.monthlyTokens` — enforced in Slice 2b by the
    /// UTC-calendar `TokenBudgetTracker`. `None` means no monthly
    /// enforcement (the env-driven legacy path never had a monthly
    /// cap).
    pub monthly_tokens: Option<u64>,

    /// `spec.contentSafety` — enforced in Slice 2c by
    /// [`crate::safety::enforce_floor`] inside the chat-completions
    /// post-response pipeline. Inactive default (all `None`,
    /// `require_prompt_shields=false`) when the CR omits the block —
    /// the floor short-circuits, leaving Slice 2a/2b behaviour
    /// unchanged.
    pub content_safety: crate::safety::ContentSafetyFloor,

    /// Whole profile JSON, kept so subsequent sub-slices can pick up
    /// other axes without a new loader.
    pub raw: serde_json::Value,
}

/// Build a fresh empty `LoadedInferencePolicyHandle`. Call once at
/// router startup and pass clones into both the loader and the
/// inference handlers.
#[must_use]
pub fn empty_handle() -> LoadedInferencePolicyHandle {
    Arc::new(RwLock::new(None))
}

/// Compute the canonical byte layout the controller hashes for a
/// single `inference-policy.json` file. Exposed as a free function so
/// the equality with the controller-side
/// `inference_policy_compile::canonical_bytes_for_digest` can be
/// asserted in a test without pulling in the loader's I/O paths.
#[must_use]
pub fn canonical_bytes_for_digest(filename: &str, body: &[u8]) -> Vec<u8> {
    let name = filename.as_bytes();
    let mut canonical: Vec<u8> = Vec::with_capacity(16 + name.len() + body.len());
    canonical.extend_from_slice(&(name.len() as u64).to_be_bytes());
    canonical.extend_from_slice(name);
    canonical.extend_from_slice(&(body.len() as u64).to_be_bytes());
    canonical.extend_from_slice(body);
    canonical
}

/// Outcome of [`load_inference_policy_from_dir`]. Kept separate from a
/// generic `Result<…, anyhow::Error>` so the caller can pattern-match
/// on the "nothing mounted yet" case without log spam.
#[derive(Debug)]
pub enum LoadOutcome {
    /// File present and parsed successfully. `loaded.digest` was
    /// registered with `PolicyStatusRegistry::record_success`.
    Loaded(LoadedInferencePolicy),
    /// Mount directory missing or empty. Registry left empty for
    /// `InferencePolicy` (controller treats this as "router hasn't
    /// loaded anything yet" and keeps the CRD in `phase=Compiled`).
    NoPolicy,
    /// Mount directory exists and has a candidate file but the file
    /// could not be read or parsed. Registry recorded a `last_error`
    /// so the controller surfaces it via the Awaiting message.
    Error(String),
}

/// Load an inference policy from the canonical mount directory.
///
/// Behaviour:
/// 1. Directory missing → `LoadOutcome::NoPolicy`, registry untouched.
/// 2. Directory empty → `LoadOutcome::NoPolicy`, registry untouched.
/// 3. File present but unreadable or non-JSON →
///    `LoadOutcome::Error(_)` + `policy_status.record_error(...)`.
/// 4. File parsed → `LoadOutcome::Loaded(_)` +
///    `policy_status.record_success(...)` with the length-prefixed
///    canonical bytes.
///
/// The loader takes a snapshot — callers wanting hot-reload must call
/// this again. Slice 2a does not yet implement file-watcher reloads.
pub fn load_inference_policy_from_dir(
    dir: &str,
    policy_status: &PolicyStatusRegistry,
) -> LoadOutcome {
    let path = Path::new(dir);
    if !path.is_dir() {
        tracing::debug!(
            dir,
            "InferencePolicy mount not present — router runs with no perRequestTokens cap"
        );
        return LoadOutcome::NoPolicy;
    }

    let mut json_files: Vec<std::path::PathBuf> = match std::fs::read_dir(path) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect(),
        Err(e) => {
            let msg = format!("read_dir failed: {e}");
            tracing::warn!(dir, error = %e, "InferencePolicy mount read_dir failed");
            policy_status.record_error(PolicyKind::InferencePolicy, dir, &msg);
            return LoadOutcome::Error(msg);
        }
    };
    json_files.sort();
    let Some(file) = json_files.first() else {
        tracing::debug!(dir, "InferencePolicy mount is empty");
        return LoadOutcome::NoPolicy;
    };

    let file_str = file.to_string_lossy().into_owned();
    let body = match std::fs::read(file) {
        Ok(b) => b,
        Err(e) => {
            let msg = format!("read failed: {e}");
            tracing::warn!(file = %file.display(), error = %e, "InferencePolicy read failed");
            policy_status.record_error(PolicyKind::InferencePolicy, &file_str, &msg);
            return LoadOutcome::Error(msg);
        }
    };

    let parsed: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("JSON parse failed: {e}");
            tracing::warn!(file = %file.display(), error = %e, "InferencePolicy parse failed");
            policy_status.record_error(PolicyKind::InferencePolicy, &file_str, &msg);
            return LoadOutcome::Error(msg);
        }
    };

    // Extract `tokenBudget.perRequestTokens` if present. Anything
    // outside the expected shape is treated as "no cap" — the
    // compile-time schema lives on the controller side, so the
    // loader must be liberal here (defence-in-depth: a bug in the
    // compiler must not crash the router).
    let per_request_tokens = parsed
        .get("tokenBudget")
        .and_then(|tb| tb.get("perRequestTokens"))
        .and_then(|v| v.as_u64());
    let daily_tokens = parsed
        .get("tokenBudget")
        .and_then(|tb| tb.get("dailyTokens"))
        .and_then(|v| v.as_u64());
    let monthly_tokens = parsed
        .get("tokenBudget")
        .and_then(|tb| tb.get("monthlyTokens"))
        .and_then(|v| v.as_u64());

    // Slice 2c: `contentSafety` may be a JSON object, `null`, or
    // entirely absent. `from_compiled_json` accepts all three and
    // returns the always-permissive default for the latter two —
    // unknown severity strings are dropped without crashing the
    // router (defence-in-depth: a future Azure-side ladder extension
    // must not brick the data plane).
    let content_safety = crate::safety::ContentSafetyFloor::from_compiled_json(
        parsed
            .get("contentSafety")
            .unwrap_or(&serde_json::Value::Null),
    );

    // Digest layout matches controller `inference_policy_digest`:
    // length-prefixed (name, body) hashed with sha256.
    let canonical = canonical_bytes_for_digest(INFERENCE_POLICY_FILENAME, &body);
    policy_status.record_success(PolicyKind::InferencePolicy, &file_str, &canonical);
    let digest = policy_status
        .get(PolicyKind::InferencePolicy)
        .and_then(|e| e.digest)
        .unwrap_or_else(|| "sha256:".to_string());

    tracing::info!(
        file = %file.display(),
        per_request_tokens = ?per_request_tokens,
        daily_tokens = ?daily_tokens,
        monthly_tokens = ?monthly_tokens,
        content_safety_active = content_safety.is_active(),
        digest = %digest,
        "InferencePolicy loaded"
    );

    LoadOutcome::Loaded(LoadedInferencePolicy {
        digest,
        source_path: file_str,
        per_request_tokens,
        daily_tokens,
        monthly_tokens,
        content_safety,
        raw: parsed,
    })
}

/// Load and install into the shared handle in one call. Used at
/// router startup. Returns the outcome so the caller can log /
/// surface in metrics.
pub async fn load_and_install(
    dir: &str,
    policy_status: &PolicyStatusRegistry,
    handle: &LoadedInferencePolicyHandle,
) -> LoadOutcome {
    let outcome = load_inference_policy_from_dir(dir, policy_status);
    if let LoadOutcome::Loaded(ref policy) = outcome {
        *handle.write().await = Some(policy.clone());
    }
    outcome
}

/// **Latency-optimised snapshot** of every enforcement axis the
/// inference handlers consume per request (Slice 2a/2b/2c). Acquired
/// with a **single** `RwLock::read().await` and then passed around
/// by value — all fields are `Copy` or trivially `Clone` (the
/// embedded `ContentSafetyFloor` is 4 × `Option<SeverityLevel>` + a
/// `bool` + a small `String` digest), so handler-internal use is
/// allocation-free after construction.
///
/// Replaces the previous per-axis helpers (one for daily/monthly
/// tokens, one for the content-safety floor, plus a manual
/// `state.inference_policy.read()` call for the perRequestTokens
/// cap). Each helper took its own lock; on the chat-completions
/// hot path that meant **three** awaits per request just to read
/// the loaded policy. With the snapshot, one await suffices and
/// every downstream branch reads from the local struct.
#[derive(Debug, Clone, Default)]
pub struct InferencePolicySnapshot {
    /// `sha256:<hex>` digest of the loaded policy, for audit logs.
    /// Empty when no policy is loaded.
    pub digest: String,
    pub per_request_tokens: Option<u64>,
    pub daily_tokens: Option<u64>,
    pub monthly_tokens: Option<u64>,
    pub content_safety: crate::safety::ContentSafetyFloor,
}

/// Take a single read-lock snapshot of the currently-loaded policy.
/// Cheap to call on every request — `RwLock::read().await` is
/// uncontended in steady state (only the startup loader writes the
/// `Option<…>`), and the snapshot itself is tiny (<200 bytes).
///
/// Use this in handlers *instead of* the per-axis helpers above for
/// any hot-path consumer. Tests targeting only one axis can keep
/// using the focused helpers for clarity.
pub async fn current_snapshot(handle: &LoadedInferencePolicyHandle) -> InferencePolicySnapshot {
    handle
        .read()
        .await
        .as_ref()
        .map(|p| InferencePolicySnapshot {
            digest: p.digest.clone(),
            per_request_tokens: p.per_request_tokens,
            daily_tokens: p.daily_tokens,
            monthly_tokens: p.monthly_tokens,
            content_safety: p.content_safety.clone(),
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::fs;
    use tempfile::TempDir;

    fn registry() -> PolicyStatusRegistry {
        PolicyStatusRegistry::new()
    }

    fn write_profile(dir: &Path, name: &str, body: &serde_json::Value) -> std::path::PathBuf {
        let p = dir.join(name);
        fs::write(&p, serde_json::to_vec(body).unwrap()).unwrap();
        p
    }

    #[test]
    fn missing_dir_yields_no_policy_and_does_not_touch_registry() {
        let reg = registry();
        let outcome = load_inference_policy_from_dir("/nonexistent/dir", &reg);
        assert!(matches!(outcome, LoadOutcome::NoPolicy));
        assert!(reg.get(PolicyKind::InferencePolicy).is_none());
    }

    #[test]
    fn empty_dir_yields_no_policy() {
        let tmp = TempDir::new().unwrap();
        let reg = registry();
        let outcome = load_inference_policy_from_dir(tmp.path().to_str().unwrap(), &reg);
        assert!(matches!(outcome, LoadOutcome::NoPolicy));
        assert!(reg.get(PolicyKind::InferencePolicy).is_none());
    }

    #[test]
    fn loads_per_request_tokens_when_present() {
        let tmp = TempDir::new().unwrap();
        let profile = serde_json::json!({
            "appliesTo": { "sandboxName": "agent-x", "sandboxMatchLabels": {}, "action": null },
            "tokenBudget": { "perRequestTokens": 8192, "dailyTokens": null, "monthlyTokens": null },
            "contentSafety": null,
            "modelPreference": null,
            "displayName": null
        });
        write_profile(tmp.path(), INFERENCE_POLICY_FILENAME, &profile);

        let reg = registry();
        let outcome = load_inference_policy_from_dir(tmp.path().to_str().unwrap(), &reg);
        let loaded = match outcome {
            LoadOutcome::Loaded(p) => p,
            other => panic!("expected Loaded, got {other:?}"),
        };
        assert_eq!(loaded.per_request_tokens, Some(8192));
        assert!(loaded.digest.starts_with("sha256:"));
        let entry = reg
            .get(PolicyKind::InferencePolicy)
            .expect("registry entry");
        assert_eq!(entry.digest.as_deref(), Some(loaded.digest.as_str()));
        assert!(entry.last_error.is_none());
    }

    #[test]
    fn missing_per_request_tokens_yields_loaded_with_none() {
        let tmp = TempDir::new().unwrap();
        let profile = serde_json::json!({
            "appliesTo": { "sandboxName": null, "sandboxMatchLabels": {}, "action": null },
            "tokenBudget": null,
            "contentSafety": null,
            "modelPreference": null,
            "displayName": null
        });
        write_profile(tmp.path(), INFERENCE_POLICY_FILENAME, &profile);

        let reg = registry();
        let outcome = load_inference_policy_from_dir(tmp.path().to_str().unwrap(), &reg);
        match outcome {
            LoadOutcome::Loaded(p) => {
                assert_eq!(p.per_request_tokens, None);
                assert_eq!(p.daily_tokens, None);
                assert_eq!(p.monthly_tokens, None);
                assert!(p.digest.starts_with("sha256:"));
            }
            other => panic!("expected Loaded, got {other:?}"),
        }
    }

    #[test]
    fn loads_daily_and_monthly_tokens_when_present() {
        // Slice 2b additive parse: `dailyTokens` / `monthlyTokens`
        // travel side-by-side with `perRequestTokens`. The
        // post-budget tracker reads them via
        // `state.inference_policy.read().await`. The digest is
        // unchanged from Slice 2a because the controller already
        // hashed the whole `tokenBudget` block.
        let tmp = TempDir::new().unwrap();
        let profile = serde_json::json!({
            "appliesTo": { "sandboxName": "agent-x", "sandboxMatchLabels": {}, "action": null },
            "tokenBudget": {
                "perRequestTokens": 8192,
                "dailyTokens": 100_000,
                "monthlyTokens": 2_000_000
            },
            "contentSafety": null,
            "modelPreference": null,
            "displayName": null
        });
        write_profile(tmp.path(), INFERENCE_POLICY_FILENAME, &profile);

        let reg = registry();
        let outcome = load_inference_policy_from_dir(tmp.path().to_str().unwrap(), &reg);
        let loaded = match outcome {
            LoadOutcome::Loaded(p) => p,
            other => panic!("expected Loaded, got {other:?}"),
        };
        assert_eq!(loaded.per_request_tokens, Some(8192));
        assert_eq!(loaded.daily_tokens, Some(100_000));
        assert_eq!(loaded.monthly_tokens, Some(2_000_000));
        // contentSafety:null → inactive floor (preserves Slice 2a/2b
        // behaviour for policies that don't opt into 2c).
        assert!(!loaded.content_safety.is_active());
    }

    #[test]
    fn loads_content_safety_floor_when_present() {
        // Slice 2c additive parse: the loader maps the compiled
        // `contentSafety` block onto `safety::ContentSafetyFloor`.
        // Severity strings travel as the controller's PascalCase
        // form; the loader's `SeverityLevel::parse` accepts both
        // cases so the same constants flow end-to-end.
        let tmp = TempDir::new().unwrap();
        let profile = serde_json::json!({
            "appliesTo": { "sandboxName": "agent-x", "sandboxMatchLabels": {}, "action": null },
            "tokenBudget": null,
            "contentSafety": {
                "hate": "Medium",
                "selfHarm": "Low",
                "sexual": "High",
                "violence": "Low",
                "requirePromptShields": true
            },
            "modelPreference": null,
            "displayName": null
        });
        write_profile(tmp.path(), INFERENCE_POLICY_FILENAME, &profile);

        let reg = registry();
        let outcome = load_inference_policy_from_dir(tmp.path().to_str().unwrap(), &reg);
        let loaded = match outcome {
            LoadOutcome::Loaded(p) => p,
            other => panic!("expected Loaded, got {other:?}"),
        };
        let floor = &loaded.content_safety;
        assert!(floor.is_active());
        assert_eq!(floor.hate, Some(crate::safety::SeverityLevel::Medium));
        assert_eq!(floor.self_harm, Some(crate::safety::SeverityLevel::Low));
        assert_eq!(floor.sexual, Some(crate::safety::SeverityLevel::High));
        assert_eq!(floor.violence, Some(crate::safety::SeverityLevel::Low));
        assert!(floor.require_prompt_shields);
    }

    #[tokio::test]
    async fn current_snapshot_returns_default_when_handle_empty() {
        // Slice 2c latency optimisation: handlers call
        // `current_snapshot` once per request. The empty-handle path
        // must short-circuit to the all-permissive default so
        // sandboxes without a loaded `InferencePolicy` keep the
        // legacy env-fallback behaviour with zero extra cost.
        let handle: LoadedInferencePolicyHandle =
            std::sync::Arc::new(tokio::sync::RwLock::new(None));
        let snap = current_snapshot(&handle).await;
        assert!(snap.digest.is_empty());
        assert!(snap.per_request_tokens.is_none());
        assert!(snap.daily_tokens.is_none());
        assert!(snap.monthly_tokens.is_none());
        assert!(!snap.content_safety.is_active());
    }

    #[tokio::test]
    async fn current_snapshot_mirrors_loaded_policy() {
        // Every enforcement axis the handlers read must travel
        // through the snapshot. Catching a missing field here is
        // cheaper than chasing a regression in production where
        // (e.g.) `daily_tokens` silently drops to `None`.
        let policy = LoadedInferencePolicy {
            digest: "sha256:dead".into(),
            source_path: "/tmp".into(),
            per_request_tokens: Some(2000),
            daily_tokens: Some(100_000),
            monthly_tokens: Some(1_000_000),
            content_safety: crate::safety::ContentSafetyFloor {
                hate: Some(crate::safety::SeverityLevel::Medium),
                self_harm: None,
                sexual: None,
                violence: None,
                require_prompt_shields: true,
            },
            raw: serde_json::Value::Null,
        };
        let handle: LoadedInferencePolicyHandle =
            std::sync::Arc::new(tokio::sync::RwLock::new(Some(policy)));
        let snap = current_snapshot(&handle).await;
        assert_eq!(snap.digest, "sha256:dead");
        assert_eq!(snap.per_request_tokens, Some(2000));
        assert_eq!(snap.daily_tokens, Some(100_000));
        assert_eq!(snap.monthly_tokens, Some(1_000_000));
        assert!(snap.content_safety.require_prompt_shields);
        assert_eq!(
            snap.content_safety.hate,
            Some(crate::safety::SeverityLevel::Medium)
        );
    }

    #[test]
    fn malformed_json_records_error_and_returns_error_outcome() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(INFERENCE_POLICY_FILENAME), b"not json").unwrap();

        let reg = registry();
        let outcome = load_inference_policy_from_dir(tmp.path().to_str().unwrap(), &reg);
        let msg = match outcome {
            LoadOutcome::Error(m) => m,
            other => panic!("expected Error, got {other:?}"),
        };
        assert!(msg.contains("JSON parse"));
        let entry = reg
            .get(PolicyKind::InferencePolicy)
            .expect("registry entry");
        assert!(entry.digest.is_none());
        assert!(entry.last_error.is_some());
    }

    #[test]
    fn digest_matches_canonical_bytes_layout() {
        // Belt-and-braces: the digest the loader registers must equal
        // sha256 over `u64-BE(name.len()) || name || u64-BE(body.len())
        // || body`. The controller side computes the same canonical
        // bytes — if these layouts drift, the `/internal/policy-status`
        // echo becomes meaningless and `decide_enforcement_state`
        // can never reach `Confirmed`.
        let tmp = TempDir::new().unwrap();
        let profile = serde_json::json!({
            "appliesTo": { "sandboxName": null, "sandboxMatchLabels": {}, "action": null },
            "tokenBudget": { "perRequestTokens": 1024, "dailyTokens": null, "monthlyTokens": null }
        });
        write_profile(tmp.path(), INFERENCE_POLICY_FILENAME, &profile);

        let reg = registry();
        let _ = load_inference_policy_from_dir(tmp.path().to_str().unwrap(), &reg);
        let observed = reg
            .get(PolicyKind::InferencePolicy)
            .and_then(|e| e.digest)
            .expect("digest recorded");

        let body = fs::read(tmp.path().join(INFERENCE_POLICY_FILENAME)).unwrap();
        let canonical = canonical_bytes_for_digest(INFERENCE_POLICY_FILENAME, &body);
        let digest_bytes = Sha256::digest(&canonical);
        let mut hex = String::with_capacity(digest_bytes.len() * 2);
        for b in digest_bytes {
            use std::fmt::Write;
            write!(hex, "{b:02x}").unwrap();
        }
        let expected = format!("sha256:{hex}");
        assert_eq!(observed, expected);
    }

    #[test]
    fn deterministic_pick_when_multiple_json_files_present() {
        // If two files exist (e.g. mid-mirror), sort and pick the
        // first — two routers seeing the same files must echo the
        // same digest.
        let tmp = TempDir::new().unwrap();
        let a = serde_json::json!({ "tokenBudget": { "perRequestTokens": 100 } });
        let b = serde_json::json!({ "tokenBudget": { "perRequestTokens": 200 } });
        write_profile(tmp.path(), "a-old.json", &a);
        write_profile(tmp.path(), "z-new.json", &b);

        let reg = registry();
        let outcome = load_inference_policy_from_dir(tmp.path().to_str().unwrap(), &reg);
        let loaded = match outcome {
            LoadOutcome::Loaded(p) => p,
            other => panic!("expected Loaded, got {other:?}"),
        };
        // Sort yields a-old.json first.
        assert_eq!(loaded.per_request_tokens, Some(100));
    }

    #[tokio::test]
    async fn load_and_install_populates_shared_handle() {
        let tmp = TempDir::new().unwrap();
        let profile = serde_json::json!({
            "tokenBudget": { "perRequestTokens": 2048 }
        });
        write_profile(tmp.path(), INFERENCE_POLICY_FILENAME, &profile);

        let reg = registry();
        let handle = empty_handle();
        assert!(handle.read().await.is_none());
        let outcome = load_and_install(tmp.path().to_str().unwrap(), &reg, &handle).await;
        assert!(matches!(outcome, LoadOutcome::Loaded(_)));
        let guard = handle.read().await;
        assert_eq!(guard.as_ref().unwrap().per_request_tokens, Some(2048));
    }
}
