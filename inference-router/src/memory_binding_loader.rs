// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `ClawMemory` compiled-binding loader (Slice 3a).
//!
//! Reads a single JSON file from a mount directory, registers the
//! sha256 digest with the shared `PolicyStatusRegistry` under
//! `PolicyKind::Memory`, and caches the parsed binding so future
//! sub-slices can plug in real consumers (`foundry.memory.*` MCP
//! tools, Slice 3b) without re-reading the file on every request.
//!
//! ## Slice 3a scope (digest-echo only)
//!
//! Slice 3a wires the §3 "Ready ⇔ router echo" loop and nothing else:
//! the controller publishes `clawmemory-<name>-binding.binding.json`,
//! the router loads + sha256's the canonical bytes, the digest shows
//! up under `GET /internal/policy-status` with `kind: Memory`, and
//! the `claw_memory_reconciler` poller promotes `Compiled → Ready`
//! once every referencing sandbox echoes a match.
//!
//! What we deliberately do **not** wire in 3a:
//!
//! - Foundry Memory Store auto-provisioning on first use (router-side
//!   HEAD/POST against the upstream).
//! - `AuthMisconfigured` condition emission on 403 from Memory Store
//!   (project-MI vs. account-MI gotcha — see `azureclaw-deployment`
//!   skill notes).
//! - Rewiring the MCP `foundry.memory.*` tools from the chart-fed
//!   `FOUNDRY_MEMORY_STORE_ID` env to the binding lookup. Today the
//!   binding loads and the digest echoes, but no behaviour changes.
//!
//! These are tracked under Slice 3b/3c.
//!
//! ## Single-binding rule
//!
//! A sandbox references at most one `ClawMemory` via
//! `ClawSandbox.spec.memoryRef`. If the mount directory accidentally
//! contains multiple `*.json` files (e.g. during a transitional
//! mirror update), the loader picks the first one in lexicographic
//! order so behaviour stays deterministic.
//!
//! ## Digest contract (DO NOT BREAK)
//!
//! The digest layout is **byte-identical** to the controller-side
//! `claw_memory_compile::canonical_bytes_for_digest`:
//! `u64-BE(name.len()) || name || u64-BE(body.len()) || body`, then
//! sha256. `name = "binding.json"`, `body = serde_json::to_vec(&binding)`
//! (non-pretty, no trailing newline). Any divergence here silently
//! breaks the §3 echo and forever-`Compiled` clusters result.

use crate::policy_status::{PolicyKind, PolicyStatusRegistry};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Canonical filename the controller writes to the
/// `clawmemory-<name>-binding` ConfigMap. Kept in lockstep with
/// `controller::claw_memory_compile::MEMORY_BINDING_FILENAME` — the
/// byte layout is part of the wire contract.
pub const MEMORY_BINDING_FILENAME: &str = "binding.json";

/// Default mount directory. Overridable via the
/// `MEMORY_BINDING_DIR` env var (which the sandbox reconciler also
/// pushes onto the inference-router container when
/// `spec.memoryRef` is set).
pub const MEMORY_BINDING_DIR_DEFAULT: &str = "/etc/azureclaw/memory";

/// Shared handle to the currently loaded `ClawMemory` binding, or
/// `None` when no binding has been loaded (mount missing, file
/// absent, or parse failure). Wired into `routes::AppState` so
/// Slice 3b's MCP rewire can do a single read-lock snapshot.
pub type LoadedMemoryBindingHandle = Arc<RwLock<Option<LoadedMemoryBinding>>>;

/// Parsed `ClawMemory` binding cached in memory. Fields populated in
/// 3a are the bare minimum the operator needs to debug a digest
/// mismatch (`source_path`, `store_name`, `scope`); the full JSON
/// stays in `raw` so 3b's MCP rewire doesn't need a new loader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedMemoryBinding {
    /// `sha256:<hex>` digest the router echoes via
    /// `GET /internal/policy-status`. Equal to the controller's
    /// `status.compiledDigest` once the §3 loop is closed.
    pub digest: String,
    /// Filesystem path the bytes came from. Surfaced in
    /// `PolicyStatusEntry.source_path` for operator debugging.
    pub source_path: String,
    /// Upstream Foundry Memory Store name. Empty string when the
    /// compiled binding omitted the field (the controller's
    /// compile step always sets it, so empty here means a hand-rolled
    /// or schema-drifted file is on the mount).
    pub store_name: String,
    /// `scope` string from the binding (e.g. `agent:foo`).
    pub scope: String,
    /// Whole binding JSON, preserved verbatim for Slice 3b consumers.
    pub raw: serde_json::Value,
}

/// Build a fresh empty handle. Call once at router startup and pass
/// clones into the loader + any future consumer (Slice 3b).
#[must_use]
pub fn empty_handle() -> LoadedMemoryBindingHandle {
    Arc::new(RwLock::new(None))
}

/// Compute the canonical byte layout the controller hashes for a
/// single `binding.json` file. Exposed as a free function so the
/// equality with the controller-side
/// `claw_memory_compile::canonical_bytes_for_digest` can be asserted
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
            "ClawMemory mount not present — router runs without a memory binding loaded"
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
            tracing::warn!(dir, error = %e, "ClawMemory mount read_dir failed");
            policy_status.record_error(PolicyKind::Memory, dir, &msg);
            return LoadOutcome::Error(msg);
        }
    };
    json_files.sort();
    let Some(file) = json_files.first() else {
        tracing::debug!(dir, "ClawMemory mount is empty");
        return LoadOutcome::NoBinding;
    };

    let file_str = file.to_string_lossy().into_owned();
    let body = match std::fs::read(file) {
        Ok(b) => b,
        Err(e) => {
            let msg = format!("read failed: {e}");
            tracing::warn!(file = %file.display(), error = %e, "ClawMemory read failed");
            policy_status.record_error(PolicyKind::Memory, &file_str, &msg);
            return LoadOutcome::Error(msg);
        }
    };

    let parsed: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("JSON parse failed: {e}");
            tracing::warn!(file = %file.display(), error = %e, "ClawMemory parse failed");
            policy_status.record_error(PolicyKind::Memory, &file_str, &msg);
            return LoadOutcome::Error(msg);
        }
    };

    // Defence-in-depth: the controller compiler always emits these
    // two fields, but a hand-edited file mounted by an operator
    // outside the controller's reconcile loop must not crash the
    // router. Default to empty string + leave the digest echo to
    // surface the divergence.
    let store_name = parsed
        .get("storeName")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let scope = parsed
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Digest layout matches controller `claw_memory_digest`:
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
        "ClawMemory binding loaded"
    );

    LoadOutcome::Loaded(LoadedMemoryBinding {
        digest,
        source_path: file_str,
        store_name,
        scope,
        raw: parsed,
    })
}

/// Load and install into the shared handle in one call. Used at
/// router startup **and** by `spawn_memory_binding_watcher`'s
/// mtime-poll loop. Returns the outcome so the caller can log /
/// surface in metrics.
///
/// Handle-update semantics by outcome:
/// - [`LoadOutcome::Loaded`] → handle is overwritten with the new
///   binding. Slice 3b consumers see the rewired `store_name` /
///   `scope` on the very next read-lock.
/// - [`LoadOutcome::NoBinding`] → handle is **cleared**
///   (`*handle = None`). This is what makes hot-reload work when an
///   operator removes `spec.memoryRef` from a `ClawSandbox`: the
///   sandbox reconciler unmounts the ConfigMap, the watcher sees an
///   empty dir, and the router stops claiming a binding exists.
/// - [`LoadOutcome::Error`] → handle is **left intact**. A transient
///   parse error during a partial mount update (controller mid-write)
///   must not knock the data plane offline; the registry already
///   captured the error so the §3 echo loop notices the digest is
///   stale.
pub async fn load_and_install(
    dir: &str,
    policy_status: &PolicyStatusRegistry,
    handle: &LoadedMemoryBindingHandle,
) -> LoadOutcome {
    let outcome = load_memory_binding_from_dir(dir, policy_status);
    match &outcome {
        LoadOutcome::Loaded(binding) => {
            *handle.write().await = Some(binding.clone());
        }
        LoadOutcome::NoBinding => {
            *handle.write().await = None;
        }
        LoadOutcome::Error(_) => {}
    }
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
/// closes Slice 3 DoD item 4 (ClawMemory hot-reload).
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
                    "ClawMemory binding directory changed, reloading"
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
/// `binding.json`).
fn dir_max_mtime(dir: &str) -> Option<std::time::SystemTime> {
    let path = Path::new(dir);
    if !path.is_dir() {
        return None;
    }
    std::fs::read_dir(path)
        .ok()?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .max()
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
        let outcome = load_memory_binding_from_dir("/nonexistent/azureclaw/memory", &reg);
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
        let body = br#"{"storeName":"my-store","scope":"agent:demo","version":1}"#;
        std::fs::write(dir.path().join("binding.json"), body).unwrap();
        let reg = PolicyStatusRegistry::new();
        let outcome = load_memory_binding_from_dir(dir.path().to_str().unwrap(), &reg);
        let LoadOutcome::Loaded(binding) = outcome else {
            panic!("expected Loaded, got {outcome:?}");
        };
        assert_eq!(binding.store_name, "my-store");
        assert_eq!(binding.scope, "agent:demo");
        assert!(binding.digest.starts_with("sha256:"));
        let entry = reg.get(PolicyKind::Memory).unwrap();
        assert_eq!(entry.digest.as_deref(), Some(binding.digest.as_str()));
        assert!(entry.last_error.is_none());
    }

    #[test]
    fn digest_is_byte_identical_to_controller_layout() {
        // Golden vector cross-validating router ↔ controller. The
        // controller's `claw_memory_digest` computes
        // `sha256(canonical_bytes_for_digest("binding.json", body))`
        // and so does the router. Any divergence here breaks the §3
        // echo loop silently.
        use sha2::{Digest, Sha256};
        let body = br#"{"storeName":"abc"}"#;
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
    fn deterministic_pick_when_multiple_json_files_present() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("b.json"),
            br#"{"storeName":"b","scope":"agent:b"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("a.json"),
            br#"{"storeName":"a","scope":"agent:a"}"#,
        )
        .unwrap();
        let reg = PolicyStatusRegistry::new();
        let LoadOutcome::Loaded(binding) =
            load_memory_binding_from_dir(dir.path().to_str().unwrap(), &reg)
        else {
            panic!("expected Loaded");
        };
        assert_eq!(binding.store_name, "a", "should pick lexicographic first");
    }

    #[tokio::test]
    async fn load_and_install_writes_handle() {
        let dir = tempdir().unwrap();
        let body = br#"{"storeName":"s","scope":"agent:s"}"#;
        std::fs::write(dir.path().join("binding.json"), body).unwrap();
        let reg = PolicyStatusRegistry::new();
        let handle = empty_handle();
        let _ = load_and_install(dir.path().to_str().unwrap(), &reg, &handle).await;
        let snapshot = handle.read().await.clone();
        assert!(snapshot.is_some(), "handle should be populated");
        assert_eq!(snapshot.unwrap().store_name, "s");
    }

    #[tokio::test]
    async fn load_and_install_leaves_handle_empty_on_error() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("binding.json"), b"not json").unwrap();
        let reg = PolicyStatusRegistry::new();
        let handle = empty_handle();
        let _ = load_and_install(dir.path().to_str().unwrap(), &reg, &handle).await;
        assert!(
            handle.read().await.is_none(),
            "handle must stay empty on parse error"
        );
    }

    #[test]
    fn empty_handle_starts_none() {
        let h = empty_handle();
        // sync read — no other writer can hold this
        let guard = h.try_read().expect("uncontended");
        assert!(guard.is_none());
    }

    #[test]
    fn defence_in_depth_missing_fields_dont_crash() {
        // No storeName / scope at all — controller would never emit
        // this, but the router must not crash if an operator
        // hand-mounts a malformed file.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("binding.json"), br#"{"version":1}"#).unwrap();
        let reg = PolicyStatusRegistry::new();
        let LoadOutcome::Loaded(binding) =
            load_memory_binding_from_dir(dir.path().to_str().unwrap(), &reg)
        else {
            panic!("expected Loaded");
        };
        assert_eq!(binding.store_name, "");
        assert_eq!(binding.scope, "");
        assert!(binding.digest.starts_with("sha256:"));
    }

    /// Hot-reload semantics (Slice 3 DoD #4): when the file disappears
    /// between reloads, `load_and_install` must clear the in-memory
    /// handle so downstream consumers (Slice 3b's
    /// `foundry.memory.{search,update}` rewire) stop returning the
    /// stale `store_name`.
    #[tokio::test]
    async fn load_and_install_clears_handle_on_no_binding() {
        let dir = tempdir().unwrap();
        let body = br#"{"storeName":"alpha","scope":"agent:a"}"#;
        std::fs::write(dir.path().join("binding.json"), body).unwrap();
        let reg = PolicyStatusRegistry::new();
        let handle = empty_handle();

        let LoadOutcome::Loaded(_) =
            load_and_install(dir.path().to_str().unwrap(), &reg, &handle).await
        else {
            panic!("expected initial Loaded");
        };
        assert!(handle.read().await.is_some(), "handle must be populated");

        std::fs::remove_file(dir.path().join("binding.json")).unwrap();
        let outcome = load_and_install(dir.path().to_str().unwrap(), &reg, &handle).await;
        assert!(matches!(outcome, LoadOutcome::NoBinding));
        assert!(
            handle.read().await.is_none(),
            "handle must be cleared after the binding goes away"
        );
    }

    /// Transient parse errors during a partial mount update must not
    /// knock the data plane offline; the registry already captured
    /// the error so the §3 echo loop notices the digest is stale.
    #[tokio::test]
    async fn load_and_install_preserves_prior_handle_on_error() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("binding.json"),
            br#"{"storeName":"alpha","scope":"agent:a"}"#,
        )
        .unwrap();
        let reg = PolicyStatusRegistry::new();
        let handle = empty_handle();

        let _ = load_and_install(dir.path().to_str().unwrap(), &reg, &handle).await;
        let pre = handle.read().await.clone().expect("loaded once");

        std::fs::write(dir.path().join("binding.json"), b"{not json").unwrap();
        let outcome = load_and_install(dir.path().to_str().unwrap(), &reg, &handle).await;
        assert!(matches!(outcome, LoadOutcome::Error(_)));
        let post = handle.read().await.clone().expect("handle still populated");
        assert_eq!(post.digest, pre.digest, "stale-but-live policy preserved");
    }

    #[test]
    fn dir_max_mtime_returns_none_for_missing_dir() {
        assert!(dir_max_mtime("/nonexistent/azureclaw/memory").is_none());
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
            br#"{"storeName":"v1","scope":"agent:a"}"#,
        )
        .unwrap();
        let reg = PolicyStatusRegistry::new();
        let handle = empty_handle();
        let _ = load_and_install(&dir_path, &reg, &handle).await;
        let digest_v1 = handle.read().await.as_ref().unwrap().digest.clone();

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
            br#"{"storeName":"v2","scope":"agent:a"}"#,
        )
        .unwrap();

        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            let current = handle.read().await.as_ref().unwrap().digest.clone();
            if current != digest_v1 {
                return; // success — watcher detected the change
            }
        }
        panic!("watcher never picked up the mtime change");
    }
}
