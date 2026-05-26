// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
// ci:loc-ok: Slice-level module; decomposition tracked in §4.2 (see dev→main #320 promotion notes)

//! Egress allowlist loader (Slice 5c.1).
//!
//! Reads the single `allowlist.json` file the controller publishes
//! into the sandbox namespace as the `karssandbox-<name>-egress-allowlist`
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
/// `karssandbox-<name>-egress-allowlist` ConfigMap. Kept in lockstep
/// with `controller::egress_allowlist_compile::EGRESS_ALLOWLIST_FILENAME`.
pub const EGRESS_ALLOWLIST_FILENAME: &str = "allowlist.json";

/// Default mount directory. Overridable via the
/// `EGRESS_ALLOWLIST_DIR` env var (which the sandbox reconciler also
/// pushes onto the inference-router container whenever the sandbox
/// references a signed bundle or inline endpoint list).
pub const EGRESS_ALLOWLIST_DIR_DEFAULT: &str = "/etc/kars/egress";

/// Domain separator used in the length-prefixed canonical bytes when
/// hashing the merged-allowlist (`baseline ∪ approvals`) digest. Not
/// a real on-disk filename — purely a name that pins the digest
/// distinct from the baseline `allowlist.json` digest. Pinned with
/// `controller::egress_approval_compile::EGRESS_APPROVAL_MERGED_FILENAME`.
pub const EGRESS_APPROVAL_MERGED_FILENAME: &str = "merged-allowlist.json";

/// Default mount directory for the per-sandbox `EgressApproval`
/// ConfigMap (Slice 5e). The sandbox reconciler mounts
/// `karssandbox-<name>-egress-approvals` here when at least one
/// approval CR targets the sandbox; the mount is `optional: true`
/// so the directory may simply be absent when there are no grants.
pub const EGRESS_APPROVAL_DIR_DEFAULT: &str = "/etc/kars/egress-approvals";

/// Env-var override for the approval mount directory.
pub const EGRESS_APPROVAL_DIR_ENV: &str = "EGRESS_APPROVAL_DIR";

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
    /// `metadata.annotations["kars.azure.com/egress-allowlist-digest"]`
    /// once the §3 echo loop is closed.
    pub digest: String,
    /// Filesystem path the bytes came from.
    pub source_path: String,
    /// Lower-cased hostnames extracted from the bundle. Used to
    /// build the new `Blocklist` allowlist on every reload.
    pub hosts: Vec<String>,
    /// All endpoints `(host, port)` with ports preserved. Drives the
    /// merged-allowlist digest computation (Slice 5e) so the router
    /// can echo the same digest the `EgressApproval` reconciler
    /// computes when enumerating `(baseline ∪ approvals)`.
    pub endpoints: Vec<(String, u16)>,
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
    let endpoints: Vec<(String, u16)> = parsed
        .get("endpoints")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|entry| {
                    let obj = entry.as_object()?;
                    let host = obj
                        .get("host")
                        .and_then(|h| h.as_str())
                        .map(|s| s.trim().to_ascii_lowercase())
                        .filter(|s| !s.is_empty())?;
                    let port = obj
                        .get("port")
                        .and_then(|p| p.as_u64())
                        .and_then(|p| u16::try_from(p).ok())
                        .unwrap_or(443);
                    Some((host, port))
                })
                .collect()
        })
        .unwrap_or_default();
    let hosts: Vec<String> = endpoints.iter().map(|(h, _)| h.clone()).collect();

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
        endpoints,
        raw: parsed,
    })
}

/// Load + install in one call. **This is where the data-plane state
/// changes:** on every invocation we either install the new host set
/// onto the live `Blocklist` (atomic replace under a single write
/// lock) or drain it to the empty set (fail-closed).
///
/// `approval_dir`, when `Some`, is scanned for per-approval files
/// (`approval-*.json`) produced by the `EgressApproval` reconciler
/// (Slice 5e). Every active approval's hosts are unioned with the
/// baseline before installing the result on the `Blocklist`, and the
/// merged-allowlist digest is echoed under
/// [`PolicyKind::EgressApproval`]. Passing `None` (or pointing at a
/// missing directory) preserves Slice 5c.1 behaviour exactly:
/// baseline-only, no `EgressApproval` echo.
///
/// Handle-update semantics by baseline outcome:
/// - [`LoadOutcome::Loaded`] → handle + `Blocklist` allowlist
///   replaced with `(baseline ∪ approvals)`.
/// - [`LoadOutcome::NoBinding`] → handle cleared + `Blocklist`
///   allowlist drained. A sandbox with no mounted bundle gets zero
///   L7 egress regardless of how many approvals are present —
///   grants extend a missing baseline to nothing.
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
    load_and_install_with_approvals(dir, None, policy_status, handle, blocklist).await
}

/// Same as [`load_and_install`] but also scans an approvals
/// directory for `EgressApproval` grants (Slice 5e). See module-level
/// docs.
pub async fn load_and_install_with_approvals(
    dir: &str,
    approval_dir: Option<&str>,
    policy_status: &PolicyStatusRegistry,
    handle: &LoadedEgressAllowlistHandle,
    blocklist: &Blocklist,
) -> LoadOutcome {
    let outcome = load_egress_allowlist_from_dir(dir, policy_status);
    match &outcome {
        LoadOutcome::Loaded(bundle) => {
            // Read approvals (best-effort; per-file parse failures
            // are tolerated — each failure is recorded under
            // `PolicyKind::EgressApproval` so the reconciler can
            // surface the operator-visible drift via its status).
            let approval_endpoints = if let Some(adir) = approval_dir {
                load_approvals_from_dir(adir)
            } else {
                Vec::new()
            };

            // Atomic replace with the union. Sort + dedup happens
            // here too so the on-the-wire host set the Blocklist
            // enforces is identical to the digest's canonical form.
            let mut union: Vec<(String, u16)> =
                Vec::with_capacity(bundle.endpoints.len() + approval_endpoints.len());
            union.extend(bundle.endpoints.iter().cloned());
            union.extend(approval_endpoints.iter().cloned());
            union.sort();
            union.dedup();

            let merged_hosts: Vec<String> = union.iter().map(|(h, _)| h.clone()).collect();
            blocklist.replace_allowlist(merged_hosts).await;
            *handle.write().await = Some(bundle.clone());

            // Echo the merged digest under PolicyKind::EgressApproval
            // whenever an approval directory was configured — even
            // when it's empty. The empty-directory case still
            // produces a stable digest (== baseline-only digest's
            // canonical form re-wrapped under the merged domain
            // separator), so the reconciler can observe "no
            // approvals currently active" and stop waiting on
            // expired ones.
            if approval_dir.is_some() {
                let body = compile_merged_endpoints_body(&union);
                let canonical = canonical_bytes_for_digest(EGRESS_APPROVAL_MERGED_FILENAME, &body);
                let source = approval_dir.unwrap_or("");
                policy_status.record_success(PolicyKind::EgressApproval, source, &canonical);
            }
        }
        LoadOutcome::NoBinding => {
            blocklist.replace_allowlist(Vec::new()).await;
            *handle.write().await = None;
            // If approvals were configured but the baseline is
            // absent, echo an empty merged digest so the
            // reconciler sees "no enforcement" cleanly. Without
            // this the EgressApproval kind would never be echoed
            // and approvals would sit Pending forever.
            if approval_dir.is_some() {
                let body = compile_merged_endpoints_body(&[]);
                let canonical = canonical_bytes_for_digest(EGRESS_APPROVAL_MERGED_FILENAME, &body);
                let source = approval_dir.unwrap_or("");
                policy_status.record_success(PolicyKind::EgressApproval, source, &canonical);
            }
        }
        LoadOutcome::Error(_) => {}
    }
    outcome
}

/// Scan `dir` for `approval-*.json` files (the wire shape produced
/// by `controller::egress_approval_compile::compile_approval_file`)
/// and return the union of their `hosts` arrays as `(host, port)`
/// pairs. Missing directories return an empty vector — Slice 5e
/// approvals are strictly additive, so absence is the no-op case.
///
/// Per-file parse failures are logged at WARN and skipped; one bad
/// approval file must not deny the rest. The `EgressApproval`
/// reconciler observes the missing host union via its own
/// merged-digest mismatch — the operator can resolve via
/// `kubectl describe`.
fn load_approvals_from_dir(dir: &str) -> Vec<(String, u16)> {
    let path = Path::new(dir);
    if !path.is_dir() {
        return Vec::new();
    }
    let mut entries: Vec<std::path::PathBuf> = match std::fs::read_dir(path) {
        Ok(it) => it
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|ext| ext == "json")
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("approval-"))
            })
            .collect(),
        Err(e) => {
            tracing::warn!(dir, error = %e, "EgressApproval read_dir failed");
            return Vec::new();
        }
    };
    entries.sort();

    let mut endpoints: Vec<(String, u16)> = Vec::new();
    for file in entries {
        let body = match std::fs::read(&file) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(file = %file.display(), error = %e, "EgressApproval file read failed");
                continue;
            }
        };
        let parsed: serde_json::Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(file = %file.display(), error = %e, "EgressApproval parse failed");
                continue;
            }
        };
        let hosts = parsed.get("hosts").and_then(|v| v.as_array());
        if let Some(arr) = hosts {
            for entry in arr {
                if let Some(obj) = entry.as_object() {
                    let host = obj
                        .get("host")
                        .and_then(|h| h.as_str())
                        .map(|s| s.trim().to_ascii_lowercase())
                        .filter(|s| !s.is_empty());
                    let port = obj
                        .get("port")
                        .and_then(|p| p.as_u64())
                        .and_then(|p| u16::try_from(p).ok())
                        .unwrap_or(443);
                    if let Some(h) = host {
                        endpoints.push((h, port));
                    }
                }
            }
        }
    }
    endpoints
}

/// Re-encode a sorted-deduped endpoint set into the canonical JSON
/// body the merged digest hashes over. Byte-identical to
/// `controller::egress_allowlist_compile::compile_to_doc` →
/// `serde_json::to_vec`.
fn compile_merged_endpoints_body(endpoints: &[(String, u16)]) -> Vec<u8> {
    use serde_json::json;
    let mut normalized: Vec<(String, u16)> = endpoints
        .iter()
        .map(|(h, p)| (h.trim().to_ascii_lowercase(), *p))
        .filter(|(h, _)| !h.is_empty())
        .collect();
    normalized.sort();
    normalized.dedup();
    let arr: Vec<serde_json::Value> = normalized
        .into_iter()
        .map(|(h, p)| json!({ "host": h, "port": p }))
        .collect();
    let doc = json!({
        "schemaVersion": 1,
        "endpoints": arr,
    });
    serde_json::to_vec(&doc).expect("canonical JSON is always serializable")
}

/// Default poll interval. Slice 5 DoD ("router reloads ≤5s after
/// kubectl edit") is the cap.
pub const DEFAULT_WATCH_INTERVAL_SECS: u64 = 5;

/// Env-var override.
pub const WATCH_INTERVAL_ENV: &str = "EGRESS_ALLOWLIST_WATCH_INTERVAL";

/// Spawn a background task that polls `dir`'s max-mtime every
/// `EGRESS_ALLOWLIST_WATCH_INTERVAL` seconds (default 5s) and calls
/// [`load_and_install`] whenever a change is detected.
///
/// `approval_dir`, when `Some`, is polled alongside the baseline —
/// any change in either directory triggers a single
/// [`load_and_install_with_approvals`] call so the merged-allowlist
/// echo updates promptly.
pub fn spawn_egress_allowlist_watcher(
    dir: String,
    policy_status: Arc<PolicyStatusRegistry>,
    handle: LoadedEgressAllowlistHandle,
    blocklist: Blocklist,
) {
    spawn_egress_allowlist_watcher_with_approvals(dir, None, policy_status, handle, blocklist);
}

/// Variant of [`spawn_egress_allowlist_watcher`] that watches an
/// approvals directory in addition to the baseline. See
/// [`load_and_install_with_approvals`] for semantics.
pub fn spawn_egress_allowlist_watcher_with_approvals(
    dir: String,
    approval_dir: Option<String>,
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
        let mut last_baseline = dir_max_mtime(&dir);
        let mut last_approvals = approval_dir.as_deref().and_then(dir_max_mtime);
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let current_baseline = dir_max_mtime(&dir);
            let current_approvals = approval_dir.as_deref().and_then(dir_max_mtime);
            if current_baseline != last_baseline || current_approvals != last_approvals {
                tracing::info!(
                    target: "egress_allowlist_watcher",
                    dir = %dir,
                    approval_dir = ?approval_dir,
                    "EgressAllowlist or EgressApproval directory changed, reloading"
                );
                let _ = load_and_install_with_approvals(
                    &dir,
                    approval_dir.as_deref(),
                    &policy_status,
                    &handle,
                    &blocklist,
                )
                .await;
                last_baseline = current_baseline;
                last_approvals = current_approvals;
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
        let outcome = load_egress_allowlist_from_dir("/nonexistent/kars/egress", &reg);
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

    // ---- Slice 5e — EgressApproval merge path -----------------

    #[test]
    fn merged_digest_filename_distinct_from_baseline() {
        // Catches accidental reuse of the baseline domain separator.
        assert_ne!(EGRESS_APPROVAL_MERGED_FILENAME, EGRESS_ALLOWLIST_FILENAME);
    }

    #[test]
    fn merged_digest_is_byte_identical_to_controller_layout() {
        // Cross-binary parity: the canonical body and digest match
        // `controller::egress_approval_compile::merged_allowlist_digest`
        // bit-for-bit. The fixture below is mirrored verbatim in
        // `controller/src/egress_approval_compile.rs` —
        // `merged_body_byte_layout_pinned_to_insertion_order`. Drift
        // on either side breaks both tests.
        //
        // The workspace pins `serde_json` with the `preserve_order`
        // feature so both binaries serialize objects in insertion
        // order regardless of which downstream crate would otherwise
        // toggle that feature on for one binary only.
        use sha2::{Digest, Sha256};
        let endpoints = vec![("example.com".to_string(), 443u16)];
        let body = compile_merged_endpoints_body(&endpoints);
        let body_str = std::str::from_utf8(&body).unwrap();
        assert_eq!(
            body_str, r#"{"schemaVersion":1,"endpoints":[{"host":"example.com","port":443}]}"#,
            "merged-allowlist body must serialize in insertion order \
             (schemaVersion first); check Cargo.toml: serde_json must \
             have `preserve_order` enabled"
        );
        let canonical = canonical_bytes_for_digest(EGRESS_APPROVAL_MERGED_FILENAME, &body);
        let mut hex_str = String::with_capacity(64);
        for b in Sha256::digest(&canonical) {
            use std::fmt::Write;
            let _ = write!(hex_str, "{b:02x}");
        }
        assert_eq!(
            format!("sha256:{hex_str}"),
            "sha256:fe6cf9580a22eaacff45a3c8d3bb06f5f635b34c5981558b4587524c45e9c8a5"
        );
        // Determinism — re-running same input yields the same body.
        let body2 = compile_merged_endpoints_body(&endpoints);
        assert_eq!(body, body2);
    }

    #[tokio::test]
    async fn approval_dir_none_preserves_legacy_behaviour() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("allowlist.json"),
            br#"{"schemaVersion":1,"endpoints":[{"host":"a.example.com","port":443}]}"#,
        )
        .unwrap();
        let reg = PolicyStatusRegistry::new();
        let handle = empty_handle();
        let blocklist = Blocklist::disabled();
        let outcome = load_and_install_with_approvals(
            dir.path().to_str().unwrap(),
            None,
            &reg,
            &handle,
            &blocklist,
        )
        .await;
        assert!(matches!(outcome, LoadOutcome::Loaded(_)));
        // No EgressApproval echo when approvals not configured.
        assert!(reg.get(PolicyKind::EgressApproval).is_none());
        // Baseline echo still present.
        assert!(reg.get(PolicyKind::EgressAllowlist).is_some());
    }

    #[tokio::test]
    async fn approval_dir_empty_emits_baseline_only_merged_digest() {
        let dir = tempdir().unwrap();
        let adir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("allowlist.json"),
            br#"{"schemaVersion":1,"endpoints":[{"host":"a.example.com","port":443}]}"#,
        )
        .unwrap();
        let reg = PolicyStatusRegistry::new();
        let handle = empty_handle();
        let blocklist = Blocklist::disabled();
        let _ = load_and_install_with_approvals(
            dir.path().to_str().unwrap(),
            Some(adir.path().to_str().unwrap()),
            &reg,
            &handle,
            &blocklist,
        )
        .await;
        let entry = reg.get(PolicyKind::EgressApproval).unwrap();
        let digest = entry.digest.unwrap();
        // The merged digest with no approvals must equal the merged
        // digest computed over baseline-only endpoints. Compare to
        // a hand-computed reference.
        let body = compile_merged_endpoints_body(&[("a.example.com".to_string(), 443u16)]);
        use sha2::{Digest, Sha256};
        let canonical = canonical_bytes_for_digest(EGRESS_APPROVAL_MERGED_FILENAME, &body);
        let mut hex_str = String::with_capacity(64);
        for b in Sha256::digest(&canonical) {
            use std::fmt::Write;
            let _ = write!(hex_str, "{b:02x}");
        }
        assert_eq!(digest, format!("sha256:{hex_str}"));
    }

    #[tokio::test]
    async fn approval_dir_with_grant_unions_hosts_and_echoes_merged_digest() {
        let dir = tempdir().unwrap();
        let adir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("allowlist.json"),
            br#"{"schemaVersion":1,"endpoints":[{"host":"a.example.com","port":443}]}"#,
        )
        .unwrap();
        std::fs::write(
            adir.path().join("approval-incident-1.json"),
            br#"{"schemaVersion":1,"approvalName":"incident-1","sandbox":"demo","hosts":[{"host":"b.example.com","port":443}],"reason":"r","effectiveAt":"t","expiresAt":"u"}"#,
        )
        .unwrap();
        let reg = PolicyStatusRegistry::new();
        let handle = empty_handle();
        let blocklist = Blocklist::disabled();
        let _ = load_and_install_with_approvals(
            dir.path().to_str().unwrap(),
            Some(adir.path().to_str().unwrap()),
            &reg,
            &handle,
            &blocklist,
        )
        .await;

        // Both hosts should now be in the Blocklist allowlist.
        assert!(
            blocklist
                .get_allowlist()
                .await
                .iter()
                .any(|h| h == "a.example.com")
        );
        assert!(
            blocklist
                .get_allowlist()
                .await
                .iter()
                .any(|h| h == "b.example.com")
        );

        // The merged digest should equal the digest over both hosts.
        let entry = reg.get(PolicyKind::EgressApproval).unwrap();
        let body = compile_merged_endpoints_body(&[
            ("a.example.com".to_string(), 443u16),
            ("b.example.com".to_string(), 443u16),
        ]);
        use sha2::{Digest, Sha256};
        let canonical = canonical_bytes_for_digest(EGRESS_APPROVAL_MERGED_FILENAME, &body);
        let mut hex_str = String::with_capacity(64);
        for b in Sha256::digest(&canonical) {
            use std::fmt::Write;
            let _ = write!(hex_str, "{b:02x}");
        }
        assert_eq!(entry.digest.unwrap(), format!("sha256:{hex_str}"));
    }

    #[tokio::test]
    async fn approval_file_with_malformed_json_is_skipped_other_files_proceed() {
        let dir = tempdir().unwrap();
        let adir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("allowlist.json"),
            br#"{"schemaVersion":1,"endpoints":[]}"#,
        )
        .unwrap();
        std::fs::write(adir.path().join("approval-broken.json"), b"{not json").unwrap();
        std::fs::write(
            adir.path().join("approval-good.json"),
            br#"{"schemaVersion":1,"approvalName":"good","sandbox":"demo","hosts":[{"host":"good.example.com","port":443}],"reason":"r","effectiveAt":"t","expiresAt":"u"}"#,
        )
        .unwrap();
        let reg = PolicyStatusRegistry::new();
        let handle = empty_handle();
        let blocklist = Blocklist::disabled();
        let _ = load_and_install_with_approvals(
            dir.path().to_str().unwrap(),
            Some(adir.path().to_str().unwrap()),
            &reg,
            &handle,
            &blocklist,
        )
        .await;
        // The good approval must have been applied even though the
        // sibling file failed to parse.
        assert!(
            blocklist
                .get_allowlist()
                .await
                .iter()
                .any(|h| h == "good.example.com")
        );
        assert!(reg.get(PolicyKind::EgressApproval).is_some());
    }

    #[tokio::test]
    async fn approval_file_files_not_prefixed_are_ignored() {
        let dir = tempdir().unwrap();
        let adir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("allowlist.json"),
            br#"{"schemaVersion":1,"endpoints":[]}"#,
        )
        .unwrap();
        // File NOT prefixed with `approval-` — must be ignored to
        // avoid colliding with future siblings (e.g. README.md
        // could be projected into the same ConfigMap by mistake).
        std::fs::write(
            adir.path().join("random.json"),
            br#"{"hosts":[{"host":"sneaky.example.com","port":443}]}"#,
        )
        .unwrap();
        let reg = PolicyStatusRegistry::new();
        let handle = empty_handle();
        let blocklist = Blocklist::disabled();
        let _ = load_and_install_with_approvals(
            dir.path().to_str().unwrap(),
            Some(adir.path().to_str().unwrap()),
            &reg,
            &handle,
            &blocklist,
        )
        .await;
        assert!(
            !blocklist
                .get_allowlist()
                .await
                .iter()
                .any(|h| h == "sneaky.example.com")
        );
    }

    #[tokio::test]
    async fn baseline_no_binding_with_approvals_still_drains_blocklist() {
        // Even with active approvals, a missing baseline must drain
        // the L7 filter to the empty set — grants only EXTEND a
        // valid baseline, they cannot create one out of nothing.
        let dir = tempdir().unwrap();
        let adir = tempdir().unwrap();
        std::fs::write(
            adir.path().join("approval-1.json"),
            br#"{"schemaVersion":1,"approvalName":"a","sandbox":"demo","hosts":[{"host":"x.example.com","port":443}],"reason":"r","effectiveAt":"t","expiresAt":"u"}"#,
        )
        .unwrap();
        let reg = PolicyStatusRegistry::new();
        let handle = empty_handle();
        let blocklist = Blocklist::disabled();
        // Pre-seed the blocklist to non-empty so we can observe the drain.
        blocklist
            .replace_allowlist(vec!["leftover.example.com".to_string()])
            .await;
        let _ = load_and_install_with_approvals(
            dir.path().to_str().unwrap(),
            Some(adir.path().to_str().unwrap()),
            &reg,
            &handle,
            &blocklist,
        )
        .await;
        assert!(
            !blocklist
                .get_allowlist()
                .await
                .iter()
                .any(|h| h == "x.example.com")
        );
        assert!(
            !blocklist
                .get_allowlist()
                .await
                .iter()
                .any(|h| h == "leftover.example.com")
        );
        // EgressApproval echo should still be present with an empty
        // merged set so the reconciler can observe the state.
        assert!(reg.get(PolicyKind::EgressApproval).is_some());
    }
}
