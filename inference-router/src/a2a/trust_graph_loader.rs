// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! TrustGraph projection loader — file-mounted source of the
//! controller-published projection.
//!
//! ## Why a file mount, not a Kube watch
//!
//! The router runs *inside* the sandbox pod (UID 1001) under a
//! restrictive seccomp profile + iptables egress-guard (only loopback +
//! DNS for UID 1000, plus apiserver for IMDS). Watching a ConfigMap in
//! the cluster-wide `azureclaw-system` namespace would require either
//! cross-namespace RBAC or a per-sandbox mirror — both controller
//! changes that belong to **Phase F2b** (per the security-audit doc).
//!
//! For Phase F2a we keep the router side strictly read-only and pure:
//! the operator (or, in F2b, the controller) is responsible for
//! materialising the projection JSON at the path named in
//! `TRUSTGRAPH_PROJECTION_PATH`. A missing file = "no projection
//! available" = identical behavior to today (no bootstrap, AGT
//! TrustManager owns the score outright).
//!
//! ## Failure semantics
//!
//! Every failure mode is fail-closed: the loader returns
//! [`TrustGraphProjection::empty()`] and emits a `tracing::warn!` with
//! the error reason. The router never crashes on a malformed projection,
//! and an empty projection cannot influence trust scoring.

use std::path::{Path, PathBuf};

use super::trust_graph_projection::{ProjectionParseError, TrustGraphProjection};

/// Environment variable that selects the projection file path. Absent
/// or empty value → no projection loaded → no bootstrap behaviour.
pub const PROJECTION_PATH_ENV: &str = "TRUSTGRAPH_PROJECTION_PATH";

/// Errors surfaced by the loader. All variants are downgraded to
/// `info!` (no-mount case) or `warn!` (parse / I/O case) and result in
/// an empty projection — see [`load_or_empty`].
#[derive(Debug, thiserror::Error)]
pub enum LoaderError {
    #[error("projection path env var {0} not set")]
    NotConfigured(&'static str),

    #[error("projection file {path:?} could not be read: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("projection file {path:?} parse error: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: ProjectionParseError,
    },
}

/// Read a projection JSON document from `path` and parse it.
pub fn load_from_path(path: &Path) -> Result<TrustGraphProjection, LoaderError> {
    let raw = std::fs::read_to_string(path).map_err(|e| LoaderError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    TrustGraphProjection::from_json(&raw).map_err(|e| LoaderError::Parse {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Resolve the projection path from `TRUSTGRAPH_PROJECTION_PATH` and
/// load it. Any failure (env var unset, file missing, parse error) is
/// logged and an empty projection is returned. **This is the
/// production entry point** — it never propagates an error to the
/// caller because the trust-graph signal is strictly opportunistic.
pub fn load_or_empty() -> TrustGraphProjection {
    let path = match std::env::var(PROJECTION_PATH_ENV) {
        Ok(s) if !s.is_empty() => PathBuf::from(s),
        _ => {
            tracing::info!(
                env = PROJECTION_PATH_ENV,
                "TrustGraph projection not configured — proceeding without bootstrap"
            );
            return TrustGraphProjection::empty();
        }
    };

    match load_from_path(&path) {
        Ok(p) => {
            tracing::info!(
                path = %path.display(),
                vertices = p.vertex_count(),
                edges = p.edge_count(),
                input_edges = p.input_edge_count(),
                version_hash = %p.version_hash(),
                "TrustGraph projection loaded"
            );
            p
        }
        Err(LoaderError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!(
                path = %path.display(),
                "TrustGraph projection file absent — proceeding without bootstrap"
            );
            TrustGraphProjection::empty()
        }
        Err(e) => {
            tracing::warn!(error = %e, "TrustGraph projection load failed — using empty projection");
            TrustGraphProjection::empty()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn unique_tmp(suffix: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "tg-projection-{}-{}-{suffix}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        p
    }

    #[test]
    fn load_from_path_parses_valid_file() {
        let path = unique_tmp("valid");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"vertices":[],"edges":[],"versionHash":"deadbeef","inputEdgeCount":0}}"#
        )
        .unwrap();
        drop(f);

        let p = load_from_path(&path).expect("loads");
        assert_eq!(p.version_hash(), "deadbeef");
        assert!(p.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_from_path_missing_file_returns_io_error() {
        let path = unique_tmp("missing");
        match load_from_path(&path).unwrap_err() {
            LoaderError::Io { source, .. } => {
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected Io variant, got {other:?}"),
        }
    }

    #[test]
    fn load_from_path_malformed_returns_parse_error() {
        let path = unique_tmp("malformed");
        std::fs::write(&path, b"{ this is not json }").unwrap();
        match load_from_path(&path).unwrap_err() {
            LoaderError::Parse { .. } => {}
            other => panic!("expected Parse variant, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }
}
