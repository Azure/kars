// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Validated `KarsMemory` binding and authorization snapshots for MCP and REST.
//!
//! Only valid bindings publish a digest. Failed or missing required reloads
//! revoke prior grants instead of retaining stale access or falling back.
//! Sandboxes without a binding use their conventional private partition.
//!
//! ## Single-binding rule
//!
//! A sandbox references at most one `KarsMemory` via
//! `KarsSandbox.spec.memoryRef`. If the mount directory accidentally
//! contains multiple `*.json` files, the loader rejects the ambiguous
//! authorization instead of choosing one policy arbitrarily.
//!
//! ## Digest contract (DO NOT BREAK)
//!
//! The digest layout is **byte-identical** to the controller-side
//! `kars_memory_compile::canonical_bytes_for_digest`:
//! `u64-BE(name.len()) || name || u64-BE(body.len()) || body`, then
//! sha256. `name = "binding.json"`, `body = serde_json::to_vec(&binding)`
//! (non-pretty, no trailing newline). Any divergence here silently
//! breaks the §3 echo and forever-`Compiled` clusters result.

use crate::policy_status::{PolicyKind, PolicyStatusRegistry};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Canonical filename the controller writes to the
/// `karsmemory-<name>-binding` ConfigMap. Kept in lockstep with
/// `controller::kars_memory_compile::MEMORY_BINDING_FILENAME` — the
/// byte layout is part of the wire contract.
pub const MEMORY_BINDING_FILENAME: &str = "binding.json";

/// Default mount directory. Overridable via the
/// `MEMORY_BINDING_DIR` env var (which the sandbox reconciler also
/// pushes onto the inference-router container when
/// `spec.memoryRef` is set).
pub const MEMORY_BINDING_DIR_DEFAULT: &str = "/etc/kars/memory";

/// `Ok(None)` means no optional binding; `Err` means memory is unavailable.
/// A failed reload replaces prior authorization, including explicit shares.
pub type LoadedMemoryBindingHandle = Arc<RwLock<Result<Option<LoadedMemoryBinding>, String>>>;

/// Validated binding, grants, and provenance cached together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedMemoryBinding {
    /// `sha256:<hex>` digest the router echoes via
    /// `GET /internal/policy-status`. Equal to the controller's
    /// `status.compiledDigest` once the §3 loop is closed.
    pub digest: String,
    /// Filesystem path the bytes came from. Surfaced in
    /// `PolicyStatusEntry.source_path` for operator debugging.
    pub source_path: String,
    /// Validated upstream Foundry Memory Store name.
    pub store_name: String,
    /// Exact private partition, without normalization.
    pub scope: String,
    pub shared_scopes: Vec<String>,
    /// Whole binding JSON, preserved verbatim for Slice 3b consumers.
    pub raw: serde_json::Value,
}

/// Build a fresh empty handle. Call once at router startup and pass
/// clones into the loader + any future consumer (Slice 3b).
#[must_use]
pub fn empty_handle() -> LoadedMemoryBindingHandle {
    Arc::new(RwLock::new(Ok(None)))
}

/// Resolve one coherent snapshot. Missing optional bindings use private conventional
/// memory; unavailable required bindings never fall back to a different partition.
pub async fn resolve_access(
    handle: Option<&LoadedMemoryBindingHandle>,
    sandbox: &str,
    fallback_store: &str,
) -> Result<crate::memory_access::MemoryAccess, String> {
    if let Some(handle) = handle {
        match &*handle.read().await {
            Ok(Some(binding)) => {
                let access = crate::memory_access::MemoryAccess {
                    store_name: binding.store_name.clone(),
                    scope: binding.scope.clone(),
                    shared_scopes: binding.shared_scopes.clone(),
                };
                access.validate().map_err(String::from)?;
                return Ok(access);
            }
            Err(message) => return Err(message.clone()),
            Ok(None) => {}
        }
    }
    if std::env::var("MEMORY_BINDING_REQUIRED").as_deref() == Ok("true") {
        return Err("required memory scope binding is unavailable".into());
    }
    let sanitize = |value: &str| -> String {
        value.chars().map(|c| if c.is_ascii_alphanumeric() || "_-.%+@/".contains(c) { c } else { '_' }).collect()
    };
    let sandbox = sanitize(sandbox.trim());
    let scope = match std::env::var("CLUSTER_NAME").ok().filter(|c| !c.trim().is_empty()) {
        Some(cluster) => format!("agent_{}_{}", sanitize(cluster.trim()), sandbox),
        None => format!("agent_{sandbox}"),
    };
    let access = crate::memory_access::MemoryAccess {
        store_name: fallback_store.to_string(), scope, shared_scopes: Vec::new(),
    };
    access.validate().map_err(String::from)?;
    Ok(access)
}

/// Compute the canonical byte layout the controller hashes for a
/// single `binding.json` file. Exposed as a free function so the
/// equality with the controller-side
/// `kars_memory_compile::canonical_bytes_for_digest` can be asserted
/// in a test without pulling in the loader's I/O paths.
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

/// Outcome of [`load_memory_binding_from_dir`]. Mirrors
/// `inference_policy_loader::LoadOutcome` so test fixtures and call
/// sites stay structurally similar.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum LoadOutcome {
    /// File present and parsed successfully. `loaded.digest` was
    /// registered with `PolicyStatusRegistry::record_success`.
    Loaded(LoadedMemoryBinding),
    /// Mount directory missing or empty. Registry left empty for
    /// `PolicyKind::Memory` — controller treats this as "router
    /// hasn't loaded anything yet" and keeps the CRD in
    /// `phase=Compiled`.
    NoBinding,
    /// Directory exists, candidate file present, but read/parse
    /// failed. Registry recorded a `last_error` so the controller
    /// surfaces it via the Awaiting message.
    Error(String),
}

/// Load a memory binding from `dir`.
///
/// Behaviour:
/// 1. Directory missing → [`LoadOutcome::NoBinding`], registry
///    untouched.
/// 2. Directory empty → [`LoadOutcome::NoBinding`], registry
///    untouched.
/// 3. File present but unreadable / non-JSON →
///    [`LoadOutcome::Error`] + `policy_status.record_error(...)`.
/// 4. File parsed → [`LoadOutcome::Loaded`] +
///    `policy_status.record_success(...)` with the canonical
///    length-prefixed bytes.
///
/// Snapshot semantics — to pick up a new binding, call again.
pub fn load_memory_binding_from_dir(
    dir: &str,
    policy_status: &PolicyStatusRegistry,
) -> LoadOutcome {
    let path = Path::new(dir);
    if !path.is_dir() {
        tracing::debug!(
            dir,
            "KarsMemory mount not present — router runs without a memory binding loaded"
        );
        return LoadOutcome::NoBinding;
    }

    let mut json_files: Vec<std::path::PathBuf> = match std::fs::read_dir(path) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect(),
        Err(e) => {
            let msg = format!("read_dir failed: {e}");
            tracing::warn!(dir, error = %e, "KarsMemory mount read_dir failed");
            policy_status.record_error(PolicyKind::Memory, dir, &msg);
            return LoadOutcome::Error(msg);
        }
    };
    json_files.sort();
    if json_files.len() > 1 {
        let message = "memory scope binding is ambiguous: expected one JSON file".to_string();
        policy_status.record_error(PolicyKind::Memory, dir, &message);
        return LoadOutcome::Error(message);
    }
    let Some(file) = json_files.first() else {
        tracing::debug!(dir, "KarsMemory mount is empty");
        return LoadOutcome::NoBinding;
    };

    let file_str = file.to_string_lossy().into_owned();
    let body = match std::fs::read(file) {
        Ok(b) => b,
        Err(e) => {
            let msg = format!("read failed: {e}");
            tracing::warn!(file = %file.display(), error = %e, "KarsMemory read failed");
            policy_status.record_error(PolicyKind::Memory, &file_str, &msg);
            return LoadOutcome::Error(msg);
        }
    };

    let parsed: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("JSON parse failed: {e}");
            tracing::warn!(file = %file.display(), error = %e, "KarsMemory parse failed");
            policy_status.record_error(PolicyKind::Memory, &file_str, &msg);
            return LoadOutcome::Error(msg);
        }
    };

    let access = serde_json::from_value::<crate::memory_access::MemoryAccess>(parsed.clone())
        .map_err(|error| error.to_string())
        .and_then(|access| access.validate().map(|()| access.clone()).map_err(String::from));
    let access = match access {
        Ok(access) => access,
        Err(message) => {
            policy_status.record_error(PolicyKind::Memory, &file_str, &message);
            return LoadOutcome::Error(message);
        }
    };
    let store_name = access.store_name;
    let scope = access.scope;

    // Digest layout matches controller `kars_memory_digest`:
    // length-prefixed (name, body) hashed with sha256.
    let canonical = canonical_bytes_for_digest(MEMORY_BINDING_FILENAME, &body);
    policy_status.record_success(PolicyKind::Memory, &file_str, &canonical);
    let digest = policy_status
        .get(PolicyKind::Memory)
        .and_then(|e| e.digest)
        .unwrap_or_else(|| "sha256:".to_string());

    tracing::info!(
        file = %file.display(),
        store_name = %store_name,
        scope = %scope,
        digest = %digest,
        "KarsMemory binding loaded"
    );

    LoadOutcome::Loaded(LoadedMemoryBinding {
        digest,
        source_path: file_str,
        store_name,
        scope,
        shared_scopes: access.shared_scopes,
        raw: parsed,
    })
}

/// Load and install into the shared handle in one call. Used at
/// router startup **and** by `spawn_memory_binding_watcher`'s
/// mtime-poll loop. Returns the outcome so the caller can log /
/// surface in metrics.
///
/// Valid reloads replace the policy. Errors revoke prior grants immediately.
/// Missing files are errors when a binding is required or was previously loaded;
/// only a never-bound optional handle retains conventional private memory.
pub async fn load_and_install(
    dir: &str,
    policy_status: &PolicyStatusRegistry,
    handle: &LoadedMemoryBindingHandle,
) -> LoadOutcome {
    load_and_install_required(dir, policy_status, handle,
        std::env::var("MEMORY_BINDING_REQUIRED").as_deref() == Ok("true")).await
}

async fn load_and_install_required(
    dir: &str, policy_status: &PolicyStatusRegistry,
    handle: &LoadedMemoryBindingHandle, required: bool,
) -> LoadOutcome {
    let mut outcome = load_memory_binding_from_dir(dir, policy_status);
    let mut current = handle.write().await;
    if matches!(outcome, LoadOutcome::NoBinding)
        && (required || !matches!(*current, Ok(None)))
    {
        let message = "required memory scope binding is unavailable".to_string();
        policy_status.record_error(PolicyKind::Memory, dir, &message);
        outcome = LoadOutcome::Error(message);
    }
    *current = match &outcome {
        LoadOutcome::Loaded(binding) => Ok(Some(binding.clone())),
        LoadOutcome::NoBinding => Ok(None),
        LoadOutcome::Error(message) => Err(message.clone()),
    };
    outcome
}

/// Default poll interval for `spawn_memory_binding_watcher`. Slice 3
/// DoD #4 ("router reloads; behaviour changes within 5s of kubectl
/// edit") is the cap; the watcher itself contributes ≤ this value
/// plus a single sync `fs::read_dir` + `read` (microsecond scale on a
/// tmpfs ConfigMap mount).
pub const DEFAULT_WATCH_INTERVAL_SECS: u64 = 5;

/// Env-var override for [`DEFAULT_WATCH_INTERVAL_SECS`]. Same shape
/// as `AGT_POLICY_WATCH_INTERVAL` (whole seconds, defaults applied
/// on parse failure).
pub const WATCH_INTERVAL_ENV: &str = "MEMORY_BINDING_WATCH_INTERVAL";

/// Spawn a background task that polls `dir`'s max-mtime every
/// `MEMORY_BINDING_WATCH_INTERVAL` seconds (default 5s) and calls
/// [`load_and_install`] whenever a change is detected. Mirrors the
/// pattern in `governance::Governance::spawn_policy_watcher` and
/// closes Slice 3 DoD item 4 (KarsMemory hot-reload).
///
/// The watcher is **best-effort**: it never panics, never propagates
/// errors out of the task, and tolerates the directory not existing
/// (e.g. sandbox boots before the ConfigMap is mirrored). When the
/// mount appears later, the next tick reloads from scratch.
pub fn spawn_memory_binding_watcher(
    dir: String,
    policy_status: Arc<PolicyStatusRegistry>,
    handle: LoadedMemoryBindingHandle,
) {
    let interval_secs: u64 = std::env::var(WATCH_INTERVAL_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v: &u64| *v > 0)
        .unwrap_or(DEFAULT_WATCH_INTERVAL_SECS);

    tokio::spawn(async move {
        let mut last_mtime = dir_max_mtime(&dir);
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        ticker.tick().await; // skip the immediate first tick
        loop {
            ticker.tick().await;
            let current = dir_max_mtime(&dir);
            if current != last_mtime {
                tracing::info!(
                    target: "memory_binding_watcher",
                    dir = %dir,
                    "KarsMemory binding directory changed, reloading"
                );
                let _ = load_and_install(&dir, &policy_status, &handle).await;
                last_mtime = current;
            }
        }
    });
}

/// Get the max mtime across `*.json` files in `dir` (mirrors
/// `governance::dir_max_mtime` but scoped to JSON since the
/// controller publishes the binding as a single
/// `binding.json`). Delegates to [`crate::config_mount::dir_max_mtime`]
/// so the ConfigMap symlink-swap fix is shared.
fn dir_max_mtime(dir: &str) -> Option<std::time::SystemTime> {
    crate::config_mount::dir_max_mtime(dir, &["json"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn canonical_bytes_match_length_prefixed_layout() {
        let body = br#"{"storeName":"x","scope":"agent:x"}"#;
        let canonical = canonical_bytes_for_digest(MEMORY_BINDING_FILENAME, body);
        // 8-byte BE name length, then name bytes, then 8-byte BE
        // body length, then body bytes.
        assert_eq!(
            &canonical[..8],
            &(MEMORY_BINDING_FILENAME.len() as u64).to_be_bytes()
        );
        assert_eq!(
            &canonical[8..8 + MEMORY_BINDING_FILENAME.len()],
            MEMORY_BINDING_FILENAME.as_bytes()
        );
        let body_len_start = 8 + MEMORY_BINDING_FILENAME.len();
        assert_eq!(
            &canonical[body_len_start..body_len_start + 8],
            &(body.len() as u64).to_be_bytes()
        );
        assert_eq!(&canonical[body_len_start + 8..], body);
    }

    #[test]
    fn missing_dir_returns_no_binding_and_leaves_registry_empty() {
        let reg = PolicyStatusRegistry::new();
        let outcome = load_memory_binding_from_dir("/nonexistent/kars/memory", &reg);
        assert!(matches!(outcome, LoadOutcome::NoBinding));
        assert!(reg.get(PolicyKind::Memory).is_none());
    }

    #[test]
    fn empty_dir_returns_no_binding() {
        let dir = tempdir().unwrap();
        let reg = PolicyStatusRegistry::new();
        let outcome = load_memory_binding_from_dir(dir.path().to_str().unwrap(), &reg);
        assert!(matches!(outcome, LoadOutcome::NoBinding));
        assert!(reg.get(PolicyKind::Memory).is_none());
    }

    #[test]
    fn malformed_json_records_error() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("binding.json"), b"{not json").unwrap();
        let reg = PolicyStatusRegistry::new();
        let outcome = load_memory_binding_from_dir(dir.path().to_str().unwrap(), &reg);
        assert!(matches!(outcome, LoadOutcome::Error(_)), "got {outcome:?}");
        let entry = reg.get(PolicyKind::Memory).unwrap();
        assert!(entry.digest.is_none());
        assert!(entry.last_error.is_some());
    }

    #[test]
    fn happy_path_registers_digest_and_caches_fields() {
        let dir = tempdir().unwrap();
        let body = br#"{"storeName":"my-store","scope":"agent_demo","version":1}"#;
        std::fs::write(dir.path().join("binding.json"), body).unwrap();
        let reg = PolicyStatusRegistry::new();
        let outcome = load_memory_binding_from_dir(dir.path().to_str().unwrap(), &reg);
        let LoadOutcome::Loaded(binding) = outcome else {
            panic!("expected Loaded, got {outcome:?}");
        };
        assert_eq!(binding.store_name, "my-store");
        assert_eq!(binding.scope, "agent_demo");
        assert!(binding.digest.starts_with("sha256:"));
        let entry = reg.get(PolicyKind::Memory).unwrap();
        assert_eq!(entry.digest.as_deref(), Some(binding.digest.as_str()));
        assert!(entry.last_error.is_none());
    }

    #[test]
    fn digest_is_byte_identical_to_controller_layout() {
        // Golden vector cross-validating router ↔ controller. The
        // controller's `kars_memory_digest` computes
        // `sha256(canonical_bytes_for_digest("binding.json", body))`
        // and so does the router. Any divergence here breaks the §3
        // echo loop silently.
        use sha2::{Digest, Sha256};
        let body = br#"{"storeName":"abc","scope":"agent_abc"}"#;
        let canonical = canonical_bytes_for_digest(MEMORY_BINDING_FILENAME, body);
        let raw = Sha256::digest(&canonical);
        let mut hexstr = String::with_capacity(raw.len() * 2);
        for b in raw {
            use std::fmt::Write;
            let _ = write!(hexstr, "{b:02x}");
        }
        let expected = format!("sha256:{hexstr}");

        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("binding.json"), body).unwrap();
        let reg = PolicyStatusRegistry::new();
        let LoadOutcome::Loaded(binding) =
            load_memory_binding_from_dir(dir.path().to_str().unwrap(), &reg)
        else {
            panic!("expected Loaded");
        };
        assert_eq!(binding.digest, expected);
    }

    #[test]
    fn ambiguous_multiple_json_bindings_are_unavailable() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("b.json"),
            br#"{"storeName":"b","scope":"agent_b"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("a.json"),
            br#"{"storeName":"a","scope":"agent_a"}"#,
        )
        .unwrap();
        let reg = PolicyStatusRegistry::new();
        assert!(matches!(load_memory_binding_from_dir(dir.path().to_str().unwrap(), &reg), LoadOutcome::Error(_)));
        assert!(reg.get(PolicyKind::Memory).unwrap().digest.is_none());
    }

    #[tokio::test]
    async fn load_and_install_writes_handle() {
        let dir = tempdir().unwrap();
        let body = br#"{"storeName":"s","scope":"agent_s"}"#;
        std::fs::write(dir.path().join("binding.json"), body).unwrap();
        let reg = PolicyStatusRegistry::new();
        let handle = empty_handle();
        let _ = load_and_install(dir.path().to_str().unwrap(), &reg, &handle).await;
        let snapshot = handle.read().await.clone();
        assert!(snapshot.as_ref().unwrap().is_some(), "handle should be populated");
        assert_eq!(snapshot.unwrap().unwrap().store_name, "s");
    }

    #[tokio::test]
    async fn load_and_install_leaves_handle_empty_on_error() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("binding.json"), b"not json").unwrap();
        let reg = PolicyStatusRegistry::new();
        let handle = empty_handle();
        let _ = load_and_install(dir.path().to_str().unwrap(), &reg, &handle).await;
        assert!(handle.read().await.is_err(), "parse error must make memory unavailable");
    }

    #[test]
    fn empty_handle_starts_none() {
        let h = empty_handle();
        // sync read — no other writer can hold this
        let guard = h.try_read().expect("uncontended");
        assert!(matches!(*guard, Ok(None)));
    }

    #[test]
    fn defence_in_depth_missing_fields_dont_crash() {
        // No storeName / scope at all — controller would never emit
        // this, but the router must not crash if an operator
        // hand-mounts a malformed file.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("binding.json"), br#"{"version":1}"#).unwrap();
        let reg = PolicyStatusRegistry::new();
        assert!(matches!(load_memory_binding_from_dir(dir.path().to_str().unwrap(), &reg), LoadOutcome::Error(_)));
        assert!(reg.get(PolicyKind::Memory).unwrap().digest.is_none());
    }

    /// Hot-reload semantics (Slice 3 DoD #4): when the file disappears
    /// between reloads, `load_and_install` must clear the in-memory
    /// handle so downstream consumers (Slice 3b's
    /// `foundry.memory.{search,update}` rewire) stop returning the
    /// stale `store_name`.
    #[tokio::test]
    async fn load_and_install_clears_handle_on_no_binding() {
        let dir = tempdir().unwrap();
        let body = br#"{"storeName":"alpha","scope":"agent_a"}"#;
        std::fs::write(dir.path().join("binding.json"), body).unwrap();
        let reg = PolicyStatusRegistry::new();
        let handle = empty_handle();

        let LoadOutcome::Loaded(_) =
            load_and_install(dir.path().to_str().unwrap(), &reg, &handle).await
        else {
            panic!("expected initial Loaded");
        };
        assert!(handle.read().await.as_ref().unwrap().is_some(), "handle must be populated");

        std::fs::remove_file(dir.path().join("binding.json")).unwrap();
        let outcome = load_and_install(dir.path().to_str().unwrap(), &reg, &handle).await;
        assert!(matches!(outcome, LoadOutcome::Error(_)));
        assert!(handle.read().await.is_err(), "missing binding must not fall back");
        assert!(resolve_access(Some(&handle), "test", "fallback").await.is_err());
    }

    #[tokio::test]
    async fn load_and_install_revokes_prior_handle_on_error() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("binding.json"),
            br#"{"storeName":"alpha","scope":"agent_a"}"#,
        )
        .unwrap();
        let reg = PolicyStatusRegistry::new();
        let handle = empty_handle();

        let _ = load_and_install(dir.path().to_str().unwrap(), &reg, &handle).await;
        assert!(handle.read().await.as_ref().unwrap().is_some());

        std::fs::write(dir.path().join("binding.json"), b"{not json").unwrap();
        let outcome = load_and_install(dir.path().to_str().unwrap(), &reg, &handle).await;
        assert!(matches!(outcome, LoadOutcome::Error(_)));
        assert!(handle.read().await.is_err());
        assert!(resolve_access(Some(&handle), "test", "fallback").await.is_err());
    }

    #[tokio::test]
    async fn required_memory_binding_missing_at_startup_is_unavailable() {
        let dir = tempdir().unwrap();
        let reg = PolicyStatusRegistry::new();
        let handle = empty_handle();
        assert!(matches!(load_and_install_required(dir.path().to_str().unwrap(), &reg, &handle, true).await, LoadOutcome::Error(_)));
        assert!(resolve_access(Some(&handle), "test", "fallback").await.is_err());
        assert!(reg.get(PolicyKind::Memory).unwrap().digest.is_none());
    }

    #[tokio::test]
    async fn memory_binding_reload_replaces_and_revokes_shared_grants() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("binding.json");
        let reg = PolicyStatusRegistry::new();
        let handle = empty_handle();
        std::fs::write(&file, br#"{"storeName":"bound","scope":"private","sharedScopes":["team"]}"#).unwrap();
        load_and_install(dir.path().to_str().unwrap(), &reg, &handle).await;
        let before = reg.get(PolicyKind::Memory).unwrap().digest;
        assert!(resolve_access(Some(&handle), "test", "fallback").await.unwrap().authorize("team").is_ok());
        std::fs::write(&file, br#"{"storeName":"bound","scope":"private","sharedScopes":[]}"#).unwrap();
        load_and_install(dir.path().to_str().unwrap(), &reg, &handle).await;
        let policy = resolve_access(Some(&handle), "test", "fallback").await.unwrap();
        assert!(policy.authorize("team").is_err());
        assert!(policy.authorize("private").is_ok());
        assert_ne!(before, reg.get(PolicyKind::Memory).unwrap().digest);
    }

    #[test]
    fn invalid_memory_grants_never_register_success() {
        let dir = tempdir().unwrap();
        let reg = PolicyStatusRegistry::new();
        for grants in [serde_json::json!(["*"]), serde_json::json!(["private"]), serde_json::json!(["team", "team"]), serde_json::json!(["../other"]), serde_json::json!([42]), serde_json::json!(null)] {
            let body = serde_json::json!({"storeName":"bound","scope":"private","sharedScopes":grants});
            std::fs::write(dir.path().join("binding.json"), body.to_string()).unwrap();
            assert!(matches!(load_memory_binding_from_dir(dir.path().to_str().unwrap(), &reg), LoadOutcome::Error(_)));
            assert!(reg.get(PolicyKind::Memory).unwrap().digest.is_none());
        }
    }

    #[test]
    fn dir_max_mtime_returns_none_for_missing_dir() {
        assert!(dir_max_mtime("/nonexistent/kars/memory").is_none());
    }

    #[test]
    fn dir_max_mtime_ignores_non_json_files() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("readme.txt"), b"x").unwrap();
        assert!(dir_max_mtime(dir.path().to_str().unwrap()).is_none());
        std::fs::write(dir.path().join("binding.json"), b"{}").unwrap();
        assert!(dir_max_mtime(dir.path().to_str().unwrap()).is_some());
    }

    /// Black-box smoke test for `spawn_memory_binding_watcher`. We
    /// can't deterministically race the 5s default ticker in a unit
    /// test, so this test crank-runs the watcher with a 1s interval
    /// (env override) and waits 2.5s after touching the file. The
    /// goal is detect-and-reload, not microsecond timing.
    #[tokio::test]
    async fn watcher_reloads_on_mtime_change() {
        let dir = tempdir().unwrap();
        let dir_path = dir.path().to_str().unwrap().to_string();
        std::fs::write(
            dir.path().join("binding.json"),
            br#"{"storeName":"v1","scope":"agent_a"}"#,
        )
        .unwrap();
        let reg = PolicyStatusRegistry::new();
        let handle = empty_handle();
        let _ = load_and_install(&dir_path, &reg, &handle).await;
        let digest_v1 = handle.read().await.as_ref().unwrap().as_ref().unwrap().digest.clone();

        // Safety: tests run sequentially within this module; setting
        // a process-wide env var is fine because no other test in
        // this file reads it.
        // SAFETY: see comment above — single-threaded test scope.
        unsafe {
            std::env::set_var(WATCH_INTERVAL_ENV, "1");
        }
        spawn_memory_binding_watcher(dir_path.clone(), Arc::new(reg), handle.clone());
        unsafe {
            std::env::remove_var(WATCH_INTERVAL_ENV);
        }

        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        // Re-create the file with new content. tmpfs mtime resolution
        // is typically 1ms but the >1s sleep above guarantees the
        // mtime moves forward observably.
        std::fs::write(
            dir.path().join("binding.json"),
            br#"{"storeName":"v2","scope":"agent_a"}"#,
        )
        .unwrap();

        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            let current = handle.read().await.as_ref().unwrap().as_ref().unwrap().digest.clone();
            if current != digest_v1 {
                return; // success — watcher detected the change
            }
        }
        panic!("watcher never picked up the mtime change");
    }
}
