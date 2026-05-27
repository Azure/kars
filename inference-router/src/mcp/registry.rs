// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Slice 4d.2 — McpServer registry (discovery half).
//!
//! The controller mirrors each `McpServer` referenced by a sandbox
//! under per-name subdirectories of `MCP_JWKS_DIR`. For example:
//!
//! ```text
//! /etc/kars/mcp/
//! ├── github/
//! │   └── jwks.json
//! ├── internal-knowledge/
//! │   └── jwks.json
//! └── foundry-builtins/
//!     └── jwks.json
//! ```
//!
//! This module discovers those subdirectories at startup and surfaces
//! the list of known server names so operators can verify the mount
//! layout from router logs and `/internal/policy-status`.
//!
//! **Scope of Slice 4d.2:** discovery + parse-time validation of each
//! `jwks.json` only. **Slice 4d.3** wires multi-JWKS OAuth verification
//! and namespaced tool dispatch — the registry built here is the
//! single source of truth those later changes consume.
//!
//! Stale-file sweep (DoD #6) is satisfied at the producer side by
//! reconciler-driven volume rebuild: each reconcile pass writes the
//! full current `mcpServerRefs` set, so volumes for removed servers
//! disappear naturally on the next pod restart. (Hot in-process removal
//! arrives in 4d.3 with the inotify watcher pattern.)

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Per-server OAuth metadata mirrored from the controller-side
/// `McpServerMeta` struct. Written to `meta.json` adjacent to
/// `jwks.json` inside each per-server subdirectory.
///
/// Slice 4d.3 consumes this to build a multi-issuer
/// `OAuthVerifierConfig` — `trusted_issuers` keyed by `issuer`,
/// `expected_audiences` aggregated from all servers' optional
/// audience fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredMcpServerMeta {
    pub issuer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    /// Slice 4d.4 — upstream MCP server URL the router's
    /// [`crate::mcp::forwarder::RouterToolDispatcher`] forwards
    /// `tools/call` requests to. Empty string when missing in
    /// `meta.json` (pre-4d.4 mirrors) — the forwarder skips such
    /// entries with a recorded reason.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// Slice 4d.4 — allowed-tools allowlist mirrored from
    /// `McpServerSpec.allowedTools`. Empty list = no tools advertised
    /// to the agent (fail-closed); `["*"]` = expose every tool the
    /// upstream advertises. Filtered against discovered upstream
    /// catalog at startup.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    /// Slice 4d.4.1 — name of an env var visible to the router that
    /// holds an outbound static bearer token. The forwarder resolves
    /// this at discovery time. Empty (the default) = no outbound auth.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bearer_from_env: String,
}

/// A single McpServer discovered under `MCP_JWKS_DIR`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredMcpServer {
    /// Server name — the subdirectory name. DNS-1123 by construction
    /// (matches the source `McpServer` CR object name).
    pub name: String,
    /// Absolute path to the discovered `jwks.json` file.
    pub jwks_path: PathBuf,
    /// Slice 4d.3 — OAuth metadata when `meta.json` is present and
    /// parses cleanly. `None` for servers mirrored before 4d.3 (pure
    /// jwks-only layout) or when the meta file is unreadable; those
    /// servers fall back to the legacy single-issuer behaviour.
    pub meta: Option<DiscoveredMcpServerMeta>,
}

/// Outcome of a single `MCP_JWKS_DIR` scan.
///
/// `servers` holds the well-formed entries. `skipped` records reasons
/// why a candidate subdirectory was not promoted — surfaced as
/// `tracing::warn!` so operators see the gap honestly (principles §3).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct McpServerRegistry {
    pub servers: BTreeMap<String, DiscoveredMcpServer>,
    pub skipped: Vec<(String, String)>,
}

impl McpServerRegistry {
    /// Number of well-formed servers.
    pub fn len(&self) -> usize {
        self.servers.len()
    }

    /// True iff no servers were discovered.
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    /// Sorted list of known server names. Cheap to compute (`BTreeMap`
    /// already maintains order). Used by `/internal/policy-status` to
    /// list addressable McpServers.
    pub fn names(&self) -> Vec<&str> {
        self.servers.keys().map(|s| s.as_str()).collect()
    }
}

/// Scan `dir` for per-server JWKS subdirectories.
///
/// Each immediate child of `dir` that is itself a directory is treated
/// as a candidate. The candidate is promoted to a `DiscoveredMcpServer`
/// iff:
///
/// 1. The subdirectory name is non-empty.
/// 2. It contains a regular file named `jwks.json`.
/// 3. That file parses as a JSON object containing a `keys` array (raw
///    RFC 7517 JWKSet shape).
///
/// Candidates that fail any of these checks are recorded in `skipped`
/// with a human-readable reason — never silently dropped (principles
/// §3). Missing or unreadable `dir` returns an empty registry with a
/// single `skipped` entry rather than an error: a sandbox with zero
/// `mcpServerRefs` is a valid steady state and should not crash the
/// router.
pub fn scan(dir: impl AsRef<Path>) -> McpServerRegistry {
    let dir = dir.as_ref();
    let mut registry = McpServerRegistry::default();

    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            registry
                .skipped
                .push((dir.display().to_string(), format!("read_dir failed: {e}")));
            return registry;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => continue,
        };

        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                registry
                    .skipped
                    .push((file_name, format!("metadata failed: {e}")));
                continue;
            }
        };
        if !meta.is_dir() {
            // Top-level files inside MCP_JWKS_DIR are not per-server
            // entries; ignore silently (e.g. could be a stray file
            // from a prior layout).
            continue;
        }

        let jwks_path = path.join("jwks.json");
        if !jwks_path.is_file() {
            registry
                .skipped
                .push((file_name, "no jwks.json inside subdirectory".to_string()));
            continue;
        }

        match fs::read_to_string(&jwks_path) {
            Ok(contents) => match serde_json::from_str::<serde_json::Value>(&contents) {
                Ok(v) => {
                    if !v.is_object() || !v.get("keys").map(|k| k.is_array()).unwrap_or(false) {
                        registry.skipped.push((
                            file_name,
                            "jwks.json is not a JWKSet (missing 'keys' array)".to_string(),
                        ));
                        continue;
                    }
                }
                Err(e) => {
                    registry
                        .skipped
                        .push((file_name, format!("jwks.json parse failed: {e}")));
                    continue;
                }
            },
            Err(e) => {
                registry
                    .skipped
                    .push((file_name, format!("jwks.json read failed: {e}")));
                continue;
            }
        }

        registry.servers.insert(
            file_name.clone(),
            DiscoveredMcpServer {
                name: file_name.clone(),
                jwks_path,
                meta: load_meta(&path, &file_name, &mut registry.skipped),
            },
        );
    }

    registry
}

/// Try to load `meta.json` adjacent to `jwks.json`. Missing file is OK
/// (returns `None` silently — back-compat with pre-4d.3 mirrors). Any
/// other error (read failure, parse failure, missing `issuer`) is
/// recorded in `skipped` so operators see the gap honestly.
fn load_meta(
    server_dir: &Path,
    server_name: &str,
    skipped: &mut Vec<(String, String)>,
) -> Option<DiscoveredMcpServerMeta> {
    let meta_path = server_dir.join("meta.json");
    if !meta_path.is_file() {
        return None;
    }
    let contents = match fs::read_to_string(&meta_path) {
        Ok(c) => c,
        Err(e) => {
            skipped.push((
                server_name.to_string(),
                format!("meta.json read failed: {e}"),
            ));
            return None;
        }
    };
    match serde_json::from_str::<DiscoveredMcpServerMeta>(&contents) {
        Ok(m) if m.url.is_empty() => {
            skipped.push((
                server_name.to_string(),
                "meta.json has empty url".to_string(),
            ));
            None
        }
        Ok(m) => Some(m),
        Err(e) => {
            skipped.push((
                server_name.to_string(),
                format!("meta.json parse failed: {e}"),
            ));
            None
        }
    }
}

/// Discover McpServers from the `MCP_JWKS_DIR` env var, logging the
/// outcome at startup. Returns the registry (empty if env var unset).
///
/// Called from `main()` exactly once after the tracing subscriber is
/// initialised. The log emission is the operator-facing surface that
/// closes Slice 4 DoD #1 — operators verify a sandbox with N
/// `mcpServerRefs` produces a "Discovered N McpServer JWKS file(s)"
/// log line listing all N names.
pub fn discover_from_env() -> McpServerRegistry {
    let dir = match std::env::var("MCP_JWKS_DIR") {
        Ok(d) if !d.trim().is_empty() => d,
        _ => {
            // Slice 4d.1 / earlier layout: legacy single-JWKS via
            // MCP_JWKS_PATH. Not an error; emit at debug only.
            tracing::debug!(
                "MCP_JWKS_DIR unset; skipping per-server McpServer discovery \
                 (legacy MCP_JWKS_PATH still honored by /mcp OAuth route)"
            );
            return McpServerRegistry::default();
        }
    };

    let registry = scan(&dir);
    if registry.is_empty() && registry.skipped.is_empty() {
        tracing::info!(
            mcp_jwks_dir = %dir,
            "MCP_JWKS_DIR is empty — no McpServers mirrored for this sandbox",
        );
    } else if !registry.is_empty() {
        let names: Vec<&str> = registry.names();
        tracing::info!(
            mcp_jwks_dir = %dir,
            count = registry.len(),
            servers = ?names,
            "Discovered {} McpServer JWKS file(s)",
            registry.len(),
        );
    }
    for (candidate, reason) in &registry.skipped {
        tracing::warn!(
            mcp_jwks_dir = %dir,
            candidate = %candidate,
            reason = %reason,
            "McpServer JWKS discovery skipped a candidate subdirectory",
        );
    }
    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_jwks(dir: &Path, server: &str, content: &str) {
        let server_dir = dir.join(server);
        fs::create_dir_all(&server_dir).unwrap();
        fs::write(server_dir.join("jwks.json"), content).unwrap();
    }

    const VALID_JWKS: &str = r#"{"keys":[{"kty":"RSA","kid":"k1","n":"abc","e":"AQAB"}]}"#;

    #[test]
    fn scan_empty_dir_returns_empty_registry() {
        let tmp = TempDir::new().unwrap();
        let registry = scan(tmp.path());
        assert!(registry.is_empty());
        assert!(registry.skipped.is_empty());
    }

    #[test]
    fn scan_missing_dir_returns_skip_entry_not_panic() {
        let registry = scan("/nonexistent/path/kars-test");
        assert!(registry.is_empty());
        assert_eq!(registry.skipped.len(), 1);
        assert!(registry.skipped[0].1.contains("read_dir failed"));
    }

    #[test]
    fn scan_discovers_three_servers_sorted() {
        let tmp = TempDir::new().unwrap();
        write_jwks(tmp.path(), "github", VALID_JWKS);
        write_jwks(tmp.path(), "internal-knowledge", VALID_JWKS);
        write_jwks(tmp.path(), "foundry-builtins", VALID_JWKS);

        let registry = scan(tmp.path());
        assert_eq!(registry.len(), 3);
        assert_eq!(
            registry.names(),
            vec!["foundry-builtins", "github", "internal-knowledge"]
        );
        assert!(registry.skipped.is_empty());
    }

    #[test]
    fn scan_skips_subdir_without_jwks_json() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("empty-server")).unwrap();
        write_jwks(tmp.path(), "good", VALID_JWKS);

        let registry = scan(tmp.path());
        assert_eq!(registry.len(), 1);
        assert!(registry.servers.contains_key("good"));
        assert_eq!(registry.skipped.len(), 1);
        assert_eq!(registry.skipped[0].0, "empty-server");
        assert!(registry.skipped[0].1.contains("no jwks.json"));
    }

    #[test]
    fn scan_skips_malformed_jwks_json() {
        let tmp = TempDir::new().unwrap();
        write_jwks(tmp.path(), "bad-json", "{not-valid-json");
        write_jwks(tmp.path(), "missing-keys", r#"{"foo":"bar"}"#);
        write_jwks(tmp.path(), "good", VALID_JWKS);

        let registry = scan(tmp.path());
        assert_eq!(registry.len(), 1);
        assert!(registry.servers.contains_key("good"));
        assert_eq!(registry.skipped.len(), 2);
        let reasons: Vec<&str> = registry.skipped.iter().map(|(_, r)| r.as_str()).collect();
        assert!(reasons.iter().any(|r| r.contains("parse failed")));
        assert!(reasons.iter().any(|r| r.contains("missing 'keys' array")));
    }

    #[test]
    fn scan_ignores_top_level_files() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("README.md"), "ignore me").unwrap();
        write_jwks(tmp.path(), "real", VALID_JWKS);

        let registry = scan(tmp.path());
        assert_eq!(registry.len(), 1);
        assert!(registry.skipped.is_empty());
    }

    #[test]
    fn discovered_server_path_points_to_jwks_file() {
        let tmp = TempDir::new().unwrap();
        write_jwks(tmp.path(), "alpha", VALID_JWKS);
        let registry = scan(tmp.path());
        let alpha = &registry.servers["alpha"];
        assert_eq!(alpha.name, "alpha");
        assert!(alpha.jwks_path.ends_with("alpha/jwks.json"));
        assert!(alpha.meta.is_none(), "no meta.json → meta should be None");
    }

    #[test]
    fn scan_loads_meta_json_when_present() {
        let tmp = TempDir::new().unwrap();
        write_jwks(tmp.path(), "github", VALID_JWKS);
        fs::write(
            tmp.path().join("github").join("meta.json"),
            r#"{"issuer":"https://idp.example/o","audience":"api://github","scopes":["mcp.tools.invoke"],"url":"https://mcp.example/v1"}"#,
        )
        .unwrap();

        let registry = scan(tmp.path());
        let github = &registry.servers["github"];
        let meta = github.meta.as_ref().expect("meta.json should load");
        assert_eq!(meta.issuer, "https://idp.example/o");
        assert_eq!(meta.audience.as_deref(), Some("api://github"));
        assert_eq!(meta.scopes, vec!["mcp.tools.invoke".to_string()]);
        assert!(registry.skipped.is_empty());
    }

    #[test]
    fn scan_records_skip_for_meta_with_empty_url() {
        let tmp = TempDir::new().unwrap();
        write_jwks(tmp.path(), "broken", VALID_JWKS);
        fs::write(
            tmp.path().join("broken").join("meta.json"),
            r#"{"issuer":"https://idp.example/o","audience":"api://x","url":""}"#,
        )
        .unwrap();

        let registry = scan(tmp.path());
        assert!(registry.servers.contains_key("broken"));
        assert!(registry.servers["broken"].meta.is_none());
        assert_eq!(registry.skipped.len(), 1);
        assert!(registry.skipped[0].1.contains("empty url"));
    }

    #[test]
    fn scan_records_skip_for_malformed_meta_json() {
        let tmp = TempDir::new().unwrap();
        write_jwks(tmp.path(), "borked", VALID_JWKS);
        fs::write(tmp.path().join("borked").join("meta.json"), "{not json").unwrap();

        let registry = scan(tmp.path());
        assert!(registry.servers.contains_key("borked"));
        assert!(registry.servers["borked"].meta.is_none());
        assert_eq!(registry.skipped.len(), 1);
        assert!(registry.skipped[0].1.contains("meta.json parse failed"));
    }
}
