// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `kars-conformance-runner` binary — Slice 6.2.
//!
//! Replays an [`kars_eval_corpus::Corpus`] of policy-conformance
//! cases against a live kars inference router and emits a
//! [`crate::report::RunReport`] JSON document describing every
//! verdict.
//!
//! The binary is intentionally pure: it has no Kubernetes API access,
//! mints no tokens, and does not touch any CR `status`. The Slice 6.3
//! `KarsEval` reconciler reads the JSON report from a shared volume
//! (or from `kubectl logs` as a fallback) and stamps the verdicts onto
//! the CR. This separation keeps the runner image small, RBAC-free,
//! and unit-testable without a kube apiserver.
//!
//! Exit codes:
//!   - `0` — corpus replayed; every case passed.
//!   - `1` — corpus replayed; at least one case failed.
//!   - `2` — hard error (could not load corpus, could not write report,
//!     runner crashed before completion).

mod cli;
mod report;
mod scenarios;
mod transport;

use anyhow::{Context, Result};
use clap::Parser;
use kars_eval_corpus::{Corpus, judge, load_builtin, parse};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use crate::cli::{Cli, CorpusSource};
use crate::report::{CaseReport, RunReport, VerdictWire, build_case_report};
use crate::scenarios::replay;
use crate::transport::Transport;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    init_tracing();

    match run(cli).await {
        Ok(any_failed) => {
            if any_failed {
                ExitCode::from(1)
            } else {
                ExitCode::from(0)
            }
        }
        Err(e) => {
            eprintln!("conformance-runner: fatal error: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn init_tracing() {
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr) // stdout is reserved for the report
        .try_init();
}

/// Returns `Ok(true)` if any case failed, `Ok(false)` if all passed.
async fn run(cli: Cli) -> Result<bool> {
    let started = chrono::Utc::now();
    let started_clock = Instant::now();

    let source = CorpusSource::parse(&cli.corpus);
    let (corpus, raw_bytes) = load_corpus(&source).context("load corpus")?;
    let digest = corpus_digest_hex(&raw_bytes);

    tracing::info!(
        corpus_name = %corpus.name,
        corpus_digest = %digest,
        cases = corpus.cases.len(),
        router_base = %cli.router_base,
        "starting conformance run",
    );

    let mut transport =
        Transport::new(cli.router_base.clone(), cli.timeout()).context("build router transport")?;

    let needs_forward_proxy = corpus
        .cases
        .iter()
        .any(|c| matches!(c.scenario, kars_eval_corpus::Scenario::EgressConnect { .. }));
    let forward_proxy_addr = cli.forward_proxy.clone().or_else(|| {
        if needs_forward_proxy {
            derive_default_forward_proxy(&cli.router_base)
        } else {
            None
        }
    });
    if let Some(addr) = forward_proxy_addr {
        tracing::info!(forward_proxy = %addr, "EgressConnect cases will tunnel through this forward proxy");
        transport = transport.with_forward_proxy(addr);
    } else if needs_forward_proxy {
        anyhow::bail!(
            "corpus contains EgressConnect scenarios but no --forward-proxy was given and router_base ({}) could not be transformed into a proxy address",
            cli.router_base
        );
    }

    let mut results: Vec<CaseReport> = Vec::with_capacity(corpus.cases.len());
    let mut passed: usize = 0;
    let mut failed: usize = 0;

    for case in &corpus.cases {
        if let Some(only) = &cli.only_case
            && &case.id != only
        {
            continue;
        }
        if let Some(only_tag) = &cli.only_tag
            && !case.tags.iter().any(|t| t == only_tag)
        {
            continue;
        }

        let case_started = Instant::now();
        let replay_result = replay(
            &transport,
            &case.scenario,
            &case.id,
            cli.auth_header.as_deref(),
        )
        .await;

        let case_report = match replay_result {
            Ok(actual) => {
                let verdict = judge(&case.expect, &actual);
                let report = build_case_report(
                    case,
                    &actual,
                    &verdict,
                    case_started.elapsed().as_millis() as u64,
                );
                match report.verdict {
                    VerdictWire::Pass => passed += 1,
                    VerdictWire::Fail { .. } => failed += 1,
                }
                report
            }
            Err(e) => {
                failed += 1;
                tracing::warn!(
                    case_id = %case.id,
                    error = %e,
                    "case replay failed at transport layer; recording as DecisionMismatch (Blocked)",
                );
                synthetic_transport_failure_report(
                    case,
                    &format!("transport error: {e:#}"),
                    case_started.elapsed().as_millis() as u64,
                )
            }
        };

        match &case_report.verdict {
            VerdictWire::Pass => tracing::info!(case_id = %case.id, "PASS"),
            VerdictWire::Fail { failure } => {
                tracing::warn!(case_id = %case.id, ?failure, "FAIL");
            }
        }

        results.push(case_report);
    }

    let completed = chrono::Utc::now();
    let duration_ms = started_clock.elapsed().as_millis() as u64;

    let total = results.len();
    let report = RunReport {
        schema_version: report::REPORT_SCHEMA_VERSION,
        corpus_name: corpus.name.clone(),
        corpus_digest: digest,
        started_at: started.to_rfc3339(),
        completed_at: completed.to_rfc3339(),
        duration_ms,
        router_base: cli.router_base.clone(),
        total,
        passed,
        failed,
        results,
    };

    let json = serde_json::to_string_pretty(&report).context("serialize report")?;
    write_report(&cli.output, &json).with_context(|| format!("write {}", cli.output.display()))?;

    if !cli.no_stdout {
        println!("{json}");
    }

    tracing::info!(
        total,
        passed,
        failed,
        duration_ms,
        "conformance run complete"
    );
    Ok(failed > 0)
}

fn load_corpus(source: &CorpusSource) -> Result<(Corpus, Vec<u8>)> {
    match source {
        CorpusSource::Builtin(name) => {
            let bytes = kars_eval_corpus::builtin_bytes(name)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "unknown built-in corpus `{name}`; available: {:?}",
                        kars_eval_corpus::BUILTIN_NAMES
                    )
                })?
                .to_vec();
            let corpus =
                load_builtin(name).map_err(|e| anyhow::anyhow!("parse built-in `{name}`: {e}"))?;
            Ok((corpus, bytes))
        }
        CorpusSource::Path(p) => {
            let bytes =
                std::fs::read(p).with_context(|| format!("read corpus file {}", p.display()))?;
            let corpus =
                parse(&bytes).map_err(|e| anyhow::anyhow!("parse {}: {e}", p.display()))?;
            Ok((corpus, bytes))
        }
    }
}

fn corpus_digest_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("sha256:{}", hex::encode(h.finalize()))
}

fn write_report(path: &PathBuf, json: &str) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create parent dir {}", parent.display()))?;
    }
    std::fs::write(path, json).context("write report file")?;
    Ok(())
}

/// Build a [`CaseReport`] that records a transport-level failure as a
/// `DecisionMismatch` (actual `Blocked` vs. whatever was expected). The
/// reason carries the transport error string so the 6.3 reconciler can
/// surface it; the corpus's `judge` function only sees actual decisions,
/// so we bypass it here and stamp the failure directly.
fn synthetic_transport_failure_report(
    case: &kars_eval_corpus::Case,
    reason: &str,
    duration_ms: u64,
) -> CaseReport {
    use kars_eval_corpus::{ActualDecision, Decision, Verdict, VerdictFailure};

    let actual = ActualDecision {
        decision: Decision::Blocked,
        by_policy_kind: None,
        reason: Some(reason.to_string()),
        observations: Vec::new(),
    };
    let verdict = Verdict::Fail(VerdictFailure::DecisionMismatch {
        expected: case.expect.decision,
        actual: Decision::Blocked,
    });
    build_case_report(case, &actual, &verdict, duration_ms)
}

/// Derive a sensible default forward-proxy address from `router_base`
/// by swapping the URL's port to `8444` (the inference router's
/// hard-coded forward-proxy port — see `inference-router/src/main.rs`
/// where `FORWARD_PROXY_PORT` defaults to 8444). Returns `None` if the
/// URL is malformed; the caller bails with a clear error in that case.
fn derive_default_forward_proxy(router_base: &str) -> Option<String> {
    let url = url::Url::parse(router_base).ok()?;
    let host = url.host_str()?;
    Some(format!("{host}:8444"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_digest_is_stable_sha256() {
        let bytes = b"hello world";
        let d = corpus_digest_hex(bytes);
        assert_eq!(
            d,
            "sha256:b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn corpus_digest_differs_for_different_bytes() {
        assert_ne!(corpus_digest_hex(b"a"), corpus_digest_hex(b"b"));
    }

    #[test]
    fn forward_proxy_derivation_swaps_port() {
        assert_eq!(
            derive_default_forward_proxy("http://router.svc:8443"),
            Some("router.svc:8444".to_string())
        );
        assert_eq!(
            derive_default_forward_proxy("https://192.0.2.10:8443"),
            Some("192.0.2.10:8444".to_string())
        );
        assert_eq!(
            derive_default_forward_proxy("http://localhost"),
            Some("localhost:8444".to_string())
        );
    }

    #[test]
    fn forward_proxy_derivation_rejects_garbage() {
        assert_eq!(derive_default_forward_proxy("not a url"), None);
    }
}
