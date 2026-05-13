// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Egress allowlist loader (Slice 5c.1).
//!
//! Reads the single `allowlist.json` file the controller publishes
//! into the sandbox namespace as the `clawsandbox-<name>-egress-allowlist`
//! ConfigMap, registers its sha256 digest with the shared
//! `PolicyStatusRegistry` under `PolicyKind::EgressAllowlist`, and
//! atomically installs the host set on the live `Blocklist` so the
//! L7 forward-proxy starts honouring the bundle on the very next
//! CONNECT / TLS-redirect.
//!
//! ## Why this exists
//!
//! Pre-5c, the signed `EgressAllowlist` artefact had **no L7 teeth**.
//! The router's `Blocklist::allowlist` was a mutable
//! `HashSet<String>` poked at by the long-deleted in-process
//! `POST /egress/approve` endpoint — there was no path from the
//! cosign-verified, controller-resolved bundle to the
//! forward-proxy's hostname filter. This loader closes that drift:
//!
//! - The bundle's bytes (compiled by
//!   `controller::egress_allowlist_compile::compile_to_doc` and
//!   length-prefixed-hashed by `egress_allowlist_digest`) are now
//!   the **sole** source of truth for what the data plane allows.
//! - There is no admin HTTP path that can mutate the in-memory set.
//!   Operator-driven runtime approvals land in Slice 5c.2 via the
//!   forthcoming `EgressApproval` CRD with its own ConfigMap mount
//!   merged in here.
//!
//! ## Fail-closed
//!
//! When the mount directory is missing, empty, or unreadable, the
//! handle is cleared **and** the live `Blocklist` allowlist is
//! atomically replaced with the empty set. Combined with the L4
//! `0.0.0.0/0 except RFC1918` NetworkPolicy on :443 (which is
//! itself reduced to a no-op by the L7 filter on the same port),
//! a sandbox with no mounted bundle is denied all egress.
//!
//! ## Digest contract (DO NOT BREAK)
//!
//! Byte-identical to `controller::egress_allowlist_compile`:
//! `u64-BE(name.len()) || name || u64-BE(body.len()) || body`, then
//! sha256. `name = "allowlist.json"`, `body = serde_json::to_vec(&doc)`
//! (non-pretty, no trailing newline). The cross-binary equality is
//! pinned by `digest_is_byte_identical_to_controller_layout` below.

use crate::blocklist::Blocklist;
use crate::policy_status::{PolicyKind, PolicyStatusRegistry};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Canonical filename the controller writes to the
/// `clawsandbox-<name>-egress-allowlist` ConfigMap. Kept in lockstep
/// with `controller::egress_allowlist_compile::EGRESS_ALLOWLIST_FILENAME`.
pub const EGRESS_ALLOWLIST_FILENAME: &str = "allowlist.json";

/// Default mount directory. Overridable via the
/// `EGRESS_ALLOWLIST_DIR` env var (which the sandbox reconciler also
/// pushes onto the inference-router container whenever the sandbox
/// references a signed bundle or inline endpoint list).
pub const EGRESS_ALLOWLIST_DIR_DEFAULT: &str = "/etc/azureclaw/egress";

/// Shared handle to the currently loaded egress allowlist, or `None`
/// when no bundle has been loaded yet (mount missing, file absent,
/// or parse failure). The watcher updates it in place on every
/// hot-reload tick.
pub type LoadedEgressAllowlistHandle = Arc<RwLock<Option<LoadedEgressAllowlist>>>;

/// Parsed egress allowlist cached in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedEgressAllowlist {
    /// `sha256:<hex>` digest the router echoes via
    /// `GET /internal/policy-status`. Equal to the controller's
    /// `metadata.annotations["azureclaw.azure.com/egress-allowlist-digest"]`
    /// once the §3 echo loop is closed.
    pub digest: String,
    /// Filesystem path the bytes came from.
    pub source_path: String,
    /// Lower-cased hostnames extracted from the bundle. Used to
    /// build the new `Blocklist` allowlist on every reload.
    pub hosts: Vec<String>,
    /// Whole bundle JSON, preserved verbatim for diagnostics + future
    /// consumers (e.g. per-endpoint port enforcement in 5c.2).
    pub raw: serde_json::Value,
}

/// Build a fresh empty handle.
#[must_use]
pub fn empty_handle() -> LoadedEgressAllowlistHandle {
    Arc::new(RwLock::new(None))
}

/// Length-prefixed canonical layout for the digest. Pinned to the
/// controller's `canonical_bytes_for_digest` byte-for-byte.
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

/// Outcome of [`load_egress_allowlist_from_dir`].
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum LoadOutcome {
    /// File present and parsed successfully.
    Loaded(LoadedEgressAllowlist),
    /// Mount directory missing or empty. Registry left empty for
    /// `PolicyKind::EgressAllowlist`. The caller (`load_and_install`)
    /// drains the live `Blocklist` allowlist so egress fails closed.
    NoBinding,
    /// Directory exists but read/parse failed. Registry recorded a
    /// `last_error`; the caller leaves the live allowlist intact so a
    /// transient mid-write blip doesn't knock the data plane offline.
    Error(String),
}

/// Pure load: read the bundle from `dir`, hash it, register the digest.
/// Does **not** mutate the `Blocklist` — see [`load_and_install`].
pub fn load_egress_allowlist_from_dir(
    dir: &str,
    policy_status: &PolicyStatusRegistry,
) -> LoadOutcome {
    let path = Path::new(dir);
    if !path.is_dir() {
        tracing::debug!(
            dir,
            "EgressAllowlist mount not present — router runs without a signed bundle"
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
            tracing::warn!(dir, error = %e, "EgressAllowlist mount read_dir failed");
            policy_status.record_error(PolicyKind::EgressAllowlist, dir, &msg);
            return LoadOutcome::Error(msg);
        }
    };
    json_files.sort();
    let Some(file) = json_files.first() else {
        tracing::debug!(dir, "EgressAllowlist mount is empty");
        return LoadOutcome::NoBinding;
    };

    let file_str = file.to_string_lossy().into_owned();
    let body = match std::fs::read(file) {
        Ok(b) => b,
        Err(e) => {
            let msg = format!("read failed: {e}");
            tracing::warn!(file = %file.display(), error = %e, "EgressAllowlist read failed");
            policy_status.record_error(PolicyKind::EgressAllowlist, &file_str, &msg);
            return LoadOutcome::Error(msg);
        }
    };

    let parsed: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("JSON parse failed: {e}");
            tracing::warn!(file = %file.display(), error = %e, "EgressAllowlist parse failed");
            policy_status.record_error(PolicyKind::EgressAllowlist, &file_str, &msg);
            return LoadOutcome::Error(msg);
        }
    };

    // Defence-in-depth: the controller compiler always emits an
    // `endpoints` array of `{host, port}`, but a hand-edited file
    // must not crash the router. Skip non-object entries and
    // non-string hosts silently — the digest echo will surface the
    // divergence.
    let hosts: Vec<String> = parsed
        .get("endpoints")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|entry| {
                    entry
                        .as_object()
                        .and_then(|m| m.get("host"))
                        .and_then(|h| h.as_str())
                        .map(|s| s.trim().to_ascii_lowercase())
                        .filter(|s| !s.is_empty())
                })
                .collect()
        })
        .unwrap_or_default();

    let canonical = canonical_bytes_for_digest(EGRESS_ALLOWLIST_FILENAME, &body);
    policy_status.record_success(PolicyKind::EgressAllowlist, &file_str, &canonical);
    let digest = policy_status
        .get(PolicyKind::EgressAllowlist)
        .and_then(|e| e.digest)
        .unwrap_or_else(|| "sha256:".to_string());

    tracing::info!(
        file = %file.display(),
        host_count = hosts.len(),
        digest = %digest,
        "EgressAllowlist bundle loaded"
    );

    LoadOutcome::Loaded(LoadedEgressAllowlist {
        digest,
        source_path: file_str,
        hosts,
        raw: parsed,
    })
}

/// Load + install in one call. **This is where the data-plane state
/// changes:** on every invocation we either install the new host set
/// onto the live `Blocklist` (atomic replace under a single write
/// lock) or drain it to the empty set (fail-closed).
///
/// Handle-update semantics by outcome:
/// - [`LoadOutcome::Loaded`] → handle + `Blocklist` allowlist
///   replaced with the new host set.
/// - [`LoadOutcome::NoBinding`] → handle cleared + `Blocklist`
///   allowlist drained. A sandbox with no mounted bundle gets zero
///   L7 egress.
/// - [`LoadOutcome::Error`] → handle and live allowlist left
///   intact. Transient parse blips during a partial mount update
///   must not knock the data plane offline; the registry already
///   captured the error so the §3 echo loop notices.
pub async fn load_and_install(
    dir: &str,
    policy_status: &PolicyStatusRegistry,
    handle: &LoadedEgressAllowlistHandle,
    blocklist: &Blocklist,
) -> LoadOutcome {
    let outcome = load_egress_allowlist_from_dir(dir, policy_status);
    match &outcome {
        LoadOutcome::Loaded(bundle) => {
            blocklist.replace_allowlist(bundle.hosts.clone()).await;
            *handle.write().await = Some(bundle.clone());
        }
        LoadOutcome::NoBinding => {
            blocklist.replace_allowlist(Vec::new()).await;
            *handle.write().await = None;
        }
        LoadOutcome::Error(_) => {}
    }
    outcome
}

/// Default poll interval. Slice 5 DoD ("router reloads ≤5s after
/// kubectl edit") is the cap.
pub const DEFAULT_WATCH_INTERVAL_SECS: u64 = 5;

/// Env-var override.
pub const WATCH_INTERVAL_ENV: &str = "EGRESS_ALLOWLIST_WATCH_INTERVAL";

/// Spawn a background task that polls `dir`'s max-mtime every
/// `EGRESS_ALLOWLIST_WATCH_INTERVAL` seconds (default 5s) and calls
/// [`load_and_install`] whenever a change is detected.
pub fn spawn_egress_allowlist_watcher(
    dir: String,
    policy_status: Arc<PolicyStatusRegistry>,
    handle: LoadedEgressAllowlistHandle,
    blocklist: Blocklist,
) {
    let interval_secs: u64 = std::env::var(WATCH_INTERVAL_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v: &u64| *v > 0)
        .unwrap_or(DEFAULT_WATCH_INTERVAL_SECS);

    tokio::spawn(async move {
        let mut last_mtime = dir_max_mtime(&dir);
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let current = dir_max_mtime(&dir);
            if current != last_mtime {
                tracing::info!(
                    target: "egress_allowlist_watcher",
                    dir = %dir,
                    "EgressAllowlist directory changed, reloading"
                );
                let _ = load_and_install(&dir, &policy_status, &handle, &blocklist).await;
                last_mtime = current;
            }
        }
    });
}

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
        let body = br#"{"schemaVersion":1,"endpoints":[]}"#;
        let canonical = canonical_bytes_for_digest(EGRESS_ALLOWLIST_FILENAME, body);
        assert_eq!(
            &canonical[..8],
            &(EGRESS_ALLOWLIST_FILENAME.len() as u64).to_be_bytes()
        );
        assert_eq!(
            &canonical[8..8 + EGRESS_ALLOWLIST_FILENAME.len()],
            EGRESS_ALLOWLIST_FILENAME.as_bytes()
        );
        let body_len_start = 8 + EGRESS_ALLOWLIST_FILENAME.len();
        assert_eq!(
            &canonical[body_len_start..body_len_start + 8],
            &(body.len() as u64).to_be_bytes()
        );
        assert_eq!(&canonical[body_len_start + 8..], body);
    }

    #[test]
    fn missing_dir_returns_no_binding_and_leaves_registry_empty() {
        let reg = PolicyStatusRegistry::new();
        let outcome = load_egress_allowlist_from_dir("/nonexistent/azureclaw/egress", &reg);
        assert!(matches!(outcome, LoadOutcome::NoBinding));
        assert!(reg.get(PolicyKind::EgressAllowlist).is_none());
    }

    #[test]
    fn empty_dir_returns_no_binding() {
        let dir = tempdir().unwrap();
        let reg = PolicyStatusRegistry::new();
        let outcome = load_egress_allowlist_from_dir(dir.path().to_str().unwrap(), &reg);
        assert!(matches!(outcome, LoadOutcome::NoBinding));
        assert!(reg.get(PolicyKind::EgressAllowlist).is_none());
    }

    #[test]
    fn malformed_json_records_error() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("allowlist.json"), b"{not json").unwrap();
        let reg = PolicyStatusRegistry::new();
        let outcome = load_egress_allowlist_from_dir(dir.path().to_str().unwrap(), &reg);
        assert!(matches!(outcome, LoadOutcome::Error(_)), "got {outcome:?}");
        let entry = reg.get(PolicyKind::EgressAllowlist).unwrap();
        assert!(entry.digest.is_none());
        assert!(entry.last_error.is_some());
    }

    #[test]
    fn happy_path_registers_digest_and_parses_hosts() {
        let dir = tempdir().unwrap();
        let body = br#"{"schemaVersion":1,"endpoints":[{"host":"api.github.com","port":443},{"host":"example.com","port":443}]}"#;
        std::fs::write(dir.path().join("allowlist.json"), body).unwrap();
        let reg = PolicyStatusRegistry::new();
        let outcome = load_egress_allowlist_from_dir(dir.path().to_str().unwrap(), &reg);
        let LoadOutcome::Loaded(bundle) = outcome else {
            panic!("expected Loaded, got {outcome:?}");
        };
        assert_eq!(bundle.hosts, vec!["api.github.com", "example.com"]);
        assert!(bundle.digest.starts_with("sha256:"));
        let entry = reg.get(PolicyKind::EgressAllowlist).unwrap();
        assert_eq!(entry.digest.as_deref(), Some(bundle.digest.as_str()));
        assert!(entry.last_error.is_none());
    }

    #[test]
    fn digest_is_byte_identical_to_controller_layout() {
        // Cross-binary parity: this digest must match
        // `controller::egress_allowlist_compile::egress_allowlist_digest`
        // bit-for-bit. Identical byte-string fixture below is
        // re-asserted in `controller/src/egress_allowlist_compile.rs`
        // — keep both in lockstep.
        use sha2::{Digest, Sha256};
        let body = br#"{"schemaVersion":1,"endpoints":[{"host":"a.example.com","port":443}]}"#;
        let canonical = canonical_bytes_for_digest(EGRESS_ALLOWLIST_FILENAME, body);
        let raw = Sha256::digest(&canonical);
        let mut hexstr = String::with_capacity(raw.len() * 2);
        for b in raw {
            use std::fmt::Write;
            let _ = write!(hexstr, "{b:02x}");
        }
        let expected = format!("sha256:{hexstr}");

        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("allowlist.json"), body).unwrap();
        let reg = PolicyStatusRegistry::new();
        let LoadOutcome::Loaded(bundle) =
            load_egress_allowlist_from_dir(dir.path().to_str().unwrap(), &reg)
        else {
            panic!("expected Loaded");
        };
        assert_eq!(bundle.digest, expected);
    }

    #[test]
    fn deterministic_pick_when_multiple_json_files_present() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("b.json"),
            br#"{"schemaVersion":1,"endpoints":[{"host":"b.example.com"}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("a.json"),
            br#"{"schemaVersion":1,"endpoints":[{"host":"a.example.com"}]}"#,
        )
        .unwrap();
        let reg = PolicyStatusRegistry::new();
        let LoadOutcome::Loaded(bundle) =
            load_egress_allowlist_from_dir(dir.path().to_str().unwrap(), &reg)
        else {
            panic!("expected Loaded");
        };
        assert_eq!(
            bundle.hosts,
            vec!["a.example.com"],
            "should pick lexicographic first"
        );
    }

    #[tokio::test]
    async fn load_and_install_writes_handle_and_blocklist() {
        let dir = tempdir().unwrap();
        let body = br#"{"schemaVersion":1,"endpoints":[{"host":"telegram.org","port":443}]}"#;
        std::fs::write(dir.path().join("allowlist.json"), body).unwrap();
        let reg = PolicyStatusRegistry::new();
        let handle = empty_handle();
        let bl = Blocklist::disabled();
        let _ = load_and_install(dir.path().to_str().unwrap(), &reg, &handle, &bl).await;

        let snapshot = handle.read().await.clone();
        assert!(snapshot.is_some(), "handle should be populated");
        let al = bl.get_allowlist().await;
        assert_eq!(al, vec!["telegram.org"]);
    }

    #[tokio::test]
    async fn load_and_install_drains_blocklist_on_no_binding() {
        let dir = tempdir().unwrap();
        // First load: install a bundle.
        let body = br#"{"schemaVersion":1,"endpoints":[{"host":"keep.example.com"}]}"#;
        std::fs::write(dir.path().join("allowlist.json"), body).unwrap();
        let reg = PolicyStatusRegistry::new();
        let handle = empty_handle();
        let bl = Blocklist::disabled();
        let _ = load_and_install(dir.path().to_str().unwrap(), &reg, &handle, &bl).await;
        assert_eq!(bl.get_allowlist().await, vec!["keep.example.com"]);

        // Second load: file removed → NoBinding → fail-closed drain.
        std::fs::remove_file(dir.path().join("allowlist.json")).unwrap();
        let out = load_and_install(dir.path().to_str().unwrap(), &reg, &handle, &bl).await;
        assert!(matches!(out, LoadOutcome::NoBinding));
        assert!(
            handle.read().await.is_none(),
            "handle must be cleared on NoBinding"
        );
        assert!(
            bl.get_allowlist().await.is_empty(),
            "live allowlist must be drained on NoBinding (fail-closed)"
        );
    }

    #[tokio::test]
    async fn load_and_install_preserves_blocklist_on_parse_error() {
        let dir = tempdir().unwrap();
        let body = br#"{"schemaVersion":1,"endpoints":[{"host":"keep.example.com"}]}"#;
        std::fs::write(dir.path().join("allowlist.json"), body).unwrap();
        let reg = PolicyStatusRegistry::new();
        let handle = empty_handle();
        let bl = Blocklist::disabled();
        let _ = load_and_install(dir.path().to_str().unwrap(), &reg, &handle, &bl).await;
        assert_eq!(bl.get_allowlist().await, vec!["keep.example.com"]);

        // Now overwrite with garbage — parse must fail and the
        // existing in-memory state must be preserved.
        std::fs::write(dir.path().join("allowlist.json"), b"{not json").unwrap();
        let out = load_and_install(dir.path().to_str().unwrap(), &reg, &handle, &bl).await;
        assert!(matches!(out, LoadOutcome::Error(_)));
        assert!(
            handle.read().await.is_some(),
            "handle must be preserved on parse error"
        );
        assert_eq!(
            bl.get_allowlist().await,
            vec!["keep.example.com"],
            "live allowlist must be preserved on parse error"
        );
    }

    #[test]
    fn empty_handle_starts_none() {
        let h = empty_handle();
        let guard = h.try_read().expect("uncontended");
        assert!(guard.is_none());
    }

    #[test]
    fn defence_in_depth_missing_endpoints_field_loads_empty() {
        // Schema-drifted file with no `endpoints` array — controller
        // would never emit this, but the router must not crash.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("allowlist.json"), br#"{"schemaVersion":1}"#).unwrap();
        let reg = PolicyStatusRegistry::new();
        let LoadOutcome::Loaded(bundle) =
            load_egress_allowlist_from_dir(dir.path().to_str().unwrap(), &reg)
        else {
            panic!("expected Loaded");
        };
        assert!(bundle.hosts.is_empty());
        assert!(bundle.digest.starts_with("sha256:"));
    }

    #[test]
    fn non_string_host_entries_are_silently_dropped() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("allowlist.json"),
            br#"{"schemaVersion":1,"endpoints":[{"host":"ok.example.com"},{"host":null},{"port":443}]}"#,
        )
        .unwrap();
        let reg = PolicyStatusRegistry::new();
        let LoadOutcome::Loaded(bundle) =
            load_egress_allowlist_from_dir(dir.path().to_str().unwrap(), &reg)
        else {
            panic!("expected Loaded");
        };
        assert_eq!(bundle.hosts, vec!["ok.example.com"]);
    }
}
