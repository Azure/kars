// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Command-line argument parsing for `azureclaw-conformance-runner`.

use clap::Parser;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "azureclaw-conformance-runner",
    about = "Replay an EvalCorpus against a live AzureClaw inference router and emit a RunReport.",
    long_about = None,
)]
pub struct Cli {
    /// Corpus source. Either `builtin:<name>` (e.g. `builtin:egress-known-bad`)
    /// or a filesystem path to a corpus JSON file.
    #[arg(long)]
    pub corpus: String,

    /// Inference-router base URL (no trailing slash required).
    /// Example: `http://router.svc.cluster.local:8443`.
    #[arg(long, value_name = "URL")]
    pub router_base: String,

    /// `host:port` of the inference router's forward proxy (default
    /// `:8444` on the same pod) used to drive `EgressConnect`
    /// scenarios via HTTP CONNECT — the only real egress entrypoint.
    /// When unset and the corpus contains `EgressConnect` scenarios,
    /// the runner derives `<router-host>:8444` from `--router-base`.
    #[arg(long, value_name = "HOST:PORT")]
    pub forward_proxy: Option<String>,

    /// Path to write the [`crate::report::RunReport`] JSON to.
    #[arg(long, value_name = "FILE")]
    pub output: PathBuf,

    /// Per-request timeout in milliseconds.
    #[arg(long, default_value_t = 5000, value_name = "MS")]
    pub timeout_ms: u64,

    /// Optional `Authorization` header forwarded with every replay request.
    /// Typically `Bearer <token>` minted by the spawning reconciler.
    #[arg(long, value_name = "HEADER")]
    pub auth_header: Option<String>,

    /// Optional filter — run only cases whose ID matches.
    #[arg(long, value_name = "CASE_ID")]
    pub only_case: Option<String>,

    /// Optional tag filter — run only cases tagged with this string.
    #[arg(long, value_name = "TAG")]
    pub only_tag: Option<String>,

    /// Suppress printing the JSON report to stdout. By default the
    /// report is written to `--output` AND echoed to stdout so it
    /// shows up in `kubectl logs`.
    #[arg(long, default_value_t = false)]
    pub no_stdout: bool,
}

impl Cli {
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }
}

/// Source resolved from the `--corpus` argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorpusSource {
    Builtin(String),
    Path(PathBuf),
}

impl CorpusSource {
    pub fn parse(s: &str) -> Self {
        if let Some(name) = s.strip_prefix("builtin:") {
            Self::Builtin(name.to_string())
        } else {
            Self::Path(PathBuf::from(s))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_source_recognises_builtin_prefix() {
        assert_eq!(
            CorpusSource::parse("builtin:egress-known-bad"),
            CorpusSource::Builtin("egress-known-bad".to_string())
        );
    }

    #[test]
    fn corpus_source_path_otherwise() {
        assert_eq!(
            CorpusSource::parse("/path/to/corpus.json"),
            CorpusSource::Path(PathBuf::from("/path/to/corpus.json"))
        );
    }

    #[test]
    fn corpus_source_relative_path() {
        assert_eq!(
            CorpusSource::parse("corpus.json"),
            CorpusSource::Path(PathBuf::from("corpus.json"))
        );
    }

    #[test]
    fn cli_timeout_converts_to_duration() {
        let cli = Cli {
            corpus: "builtin:x".into(),
            router_base: "http://x".into(),
            forward_proxy: None,
            output: PathBuf::from("/tmp/out.json"),
            timeout_ms: 2500,
            auth_header: None,
            only_case: None,
            only_tag: None,
            no_stdout: false,
        };
        assert_eq!(cli.timeout(), Duration::from_millis(2500));
    }
}
