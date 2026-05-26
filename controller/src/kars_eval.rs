// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `KarsEval` CRD — Slice 6 (`crd-well-oiled-machine`).
//!
//! `KarsEval` declares an **automated policy-conformance run** against a
//! target [`crate::crd::KarsSandbox`]. The reconciler spawns a Kubernetes
//! `Job` (or `CronJob`, when a schedule is set) running the
//! [`kars-conformance-runner`](https://crates.io/crates/kars-conformance-runner)
//! image; the runner replays a curated **eval corpus** of attacks against
//! the sandbox's inference router and emits a JSON `RunReport` to its pod
//! log. The reconciler reads the log back, parses the report, and stamps
//! per-case verdicts onto `status`.
//!
//! This replaces the pre-Slice-6 `KarsEval` shape (an unimplemented Foundry
//! Evals binding). The new shape is a real, end-to-end producer-consumer
//! loop: corpus → runner Job → log → status patch → optional sandbox
//! Degraded + signed webhook.
//!
//! ## Spec
//!
//! - [`target_sandbox_ref`](KarsEvalSpec::target_sandbox_ref) — the sandbox
//!   under test. Must live in the same namespace.
//! - [`corpus`](KarsEvalSpec::corpus) — either a `Builtin{name}` reference
//!   to one of the 5 corpora shipped in
//!   [`kars_eval_corpus::builtin`](https://docs.rs/kars-eval-corpus),
//!   or a signed `Bundle{bundle_ref}` pulled via
//!   [`crate::policy_fetcher::fetch_and_verify_generic`] with
//!   [`crate::policy_canonical::eval_corpus::EvalCorpusKind`].
//! - [`schedule`](KarsEvalSpec::schedule) — optional cron expression. When
//!   set, the reconciler ensures a `CronJob`. When unset, runs only when an
//!   operator adds the `kars.azure.com/run-now=true` annotation.
//! - [`fail_sandbox_on_drift`](KarsEvalSpec::fail_sandbox_on_drift) — when
//!   true, a failing report patches the target sandbox to `Degraded` via a
//!   distinct field manager (`kars-controller/karseval-drift`).
//!
//! Webhook delivery and operator-driven CLI surfaces (`kars eval run`)
//! ship in Slice 6.4.
//!
//! ## Status
//!
//! - [`last_run_at`](KarsEvalStatus::last_run_at) — RFC 3339 timestamp of
//!   the last completed run.
//! - [`last_result`](KarsEvalStatus::last_result) — the most recent
//!   verdict (pass/fail counts + drift summary).
//! - [`history`](KarsEvalStatus::history) — bounded to the last
//!   [`MAX_HISTORY`] runs, newest-first.
//! - [`conditions`](KarsEvalStatus::conditions) — Ready / Progressing /
//!   Degraded / `ConformanceDrift`.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::crd::OciArtifactRef;
use crate::mcp_server::LocalObjectRef;

/// Maximum number of `EvalResultSummary` entries the controller will keep
/// in `status.history`. Older entries are dropped (FIFO from the tail).
///
/// 20 is large enough to see weekly drift across a 4-month window of
/// daily runs, small enough that the entire status fits in etcd's default
/// 1 MiB object cap with thousands of bytes to spare for evidence text.
pub const MAX_HISTORY: usize = 20;

/// Annotation operators add to a `KarsEval` CR to trigger an immediate
/// run, in addition to (or instead of) the scheduled run. The controller
/// clears the annotation once it has spawned the corresponding Job, so
/// the annotation is idempotent (re-setting it triggers another run).
pub const ANNOTATION_RUN_NOW: &str = "kars.azure.com/run-now";

/// `KarsEval.spec` — declares a conformance run over a sandbox.
#[derive(CustomResource, Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[kube(
    group = "kars.azure.com",
    version = "v1alpha1",
    kind = "KarsEval",
    namespaced,
    status = "KarsEvalStatus",
    shortname = "ceval",
    printcolumn = r#"{"name":"Sandbox","type":"string","jsonPath":".spec.targetSandboxRef.name"}"#,
    printcolumn = r#"{"name":"Schedule","type":"string","jsonPath":".spec.schedule"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"LastRun","type":"date","jsonPath":".status.lastRunAt"}"#,
    printcolumn = r#"{"name":"Passed","type":"integer","jsonPath":".status.lastResult.passed"}"#,
    printcolumn = r#"{"name":"Failed","type":"integer","jsonPath":".status.lastResult.failed"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct KarsEvalSpec {
    /// Sandbox this eval applies to. Must live in the same namespace.
    pub target_sandbox_ref: SandboxRef,

    /// Source of the corpus to replay. Exactly one variant must be set;
    /// CEL admission enforces the mutex.
    pub corpus: CorpusSource,

    /// Optional cron schedule (5-token form, K8s `CronJob.spec.schedule`
    /// shape). When empty/absent, the eval runs only when an operator
    /// adds the `kars.azure.com/run-now=true` annotation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,

    /// When `true`, a failing run patches the target `KarsSandbox`'s
    /// `Degraded` condition (reason `ConformanceDrift`) via a distinct
    /// field manager. The sandbox reconciler will then refuse to admit
    /// new agent sessions until the next successful eval clears the
    /// condition.
    ///
    /// Defaults to `false` so first-time operators can adopt the CRD
    /// without immediately taking sandboxes offline on a corpus update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail_sandbox_on_drift: Option<bool>,

    /// Optional human-readable label surfaced in CLI output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,

    /// Optional override of the conformance-runner container image. When
    /// absent the controller falls back to the Helm-configured default
    /// (`KARS_CONFORMANCE_RUNNER_IMAGE` env). Setting this on a CR
    /// is an escape hatch for in-cluster development; production should
    /// pin the image globally via the Helm chart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_image: Option<String>,
}

/// Reference to the sandbox under test. Always same-namespace (no
/// cross-namespace `namespace` field is permitted today; cross-namespace
/// evals would require a separate RBAC story).
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SandboxRef {
    /// Sandbox name (`KarsSandbox.metadata.name`).
    pub name: String,
}

/// Source of the eval corpus. Mutually exclusive.
///
/// `Builtin` references one of the corpora compiled into the controller
/// binary via [`kars_eval_corpus::builtin::load`]. `Bundle` pulls a
/// signed corpus from an OCI registry; bytes are verified against the
/// kind's media type and parsed via [`crate::policy_canonical::eval_corpus`].
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CorpusSource {
    /// Name of a builtin corpus shipped with the controller. See
    /// `kars_eval_corpus::builtin::ALL_NAMES`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtin: Option<String>,

    /// Reference to a signed OCI artifact carrying a corpus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_ref: Option<OciArtifactRef>,
}

impl Default for CorpusSource {
    fn default() -> Self {
        Self {
            builtin: Some("jailbreak-baseline".into()),
            bundle_ref: None,
        }
    }
}

/// `KarsEval.status`.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KarsEvalStatus {
    /// One of: `Pending`, `Running`, `Ready`, `Degraded`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,

    /// Standard K8s conditions. Includes `Ready`, `Progressing`,
    /// `Degraded`, plus the eval-specific `ConformanceDrift` condition
    /// (`True` when the most recent run reported any failed case).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<Condition>>,

    /// RFC 3339 timestamp of the last completed run, written when the
    /// reconciler successfully reads the runner pod log and parses the
    /// `RunReport`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,

    /// Detailed result of the most recent run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result: Option<EvalResult>,

    /// Summaries of the previous [`MAX_HISTORY`] runs, newest-first. The
    /// entry at index 0 is also reflected in `last_result`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<EvalResultSummary>,

    /// Pointer to the corpus `ConfigMap` produced by the reconciler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corpus_config_map_ref: Option<LocalObjectRef>,

    /// Hex `sha256:` digest of the resolved corpus bytes (matches the
    /// digest computed by `eval_corpus::parse_corpus`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corpus_digest: Option<String>,

    /// When `spec.schedule` is set, name of the controller-owned
    /// `CronJob` that fires periodic runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron_job_name: Option<String>,
}

/// Full result of one eval run. Mirrors the runner's `RunReport` plus the
/// per-case verdict counts. Bounded in shape — the controller never
/// stamps the entire per-case detail vector (which can be huge) on
/// status; instead it stores a compact summary plus references to the
/// runner Job/Pod for replay.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EvalResult {
    /// Schema version of the runner `RunReport` consumed. Matches
    /// `kars_conformance_runner::report::REPORT_SCHEMA_VERSION`.
    pub schema_version: String,

    /// Hex `sha256:` digest of the corpus that produced this run.
    pub corpus_digest: String,

    /// Total cases the runner attempted.
    pub total: u32,

    /// Cases whose actual decision matched the expected verdict.
    pub passed: u32,

    /// Cases whose actual decision did NOT match the expected verdict.
    pub failed: u32,

    /// Cases the runner could not exercise (transport error, timeout).
    pub errored: u32,

    /// Name of the corpus (`builtin` name, or the `repository@digest`
    /// of the bundle).
    pub corpus_label: String,

    /// Name of the K8s `Job` that produced this run. Operators use this
    /// to fetch the full runner log via `kubectl logs`.
    pub job_name: String,

    /// Up to 5 first-failing case IDs surfaced for at-a-glance triage.
    /// The full per-case detail lives in the runner log.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub first_failing_cases: Vec<String>,
}

/// Compact summary kept in `status.history`. The most-recent entry
/// matches `last_result.{schema_version, corpus_digest, total, passed,
/// failed, errored, last_run_at}`.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EvalResultSummary {
    /// RFC 3339 completion timestamp.
    pub at: String,
    pub corpus_digest: String,
    pub total: u32,
    pub passed: u32,
    pub failed: u32,
    pub errored: u32,
    pub job_name: String,
}

/// Helper: prepend `summary` to `history`, capping at [`MAX_HISTORY`].
///
/// The vector is treated as a deque: newest entry at index 0, older
/// entries trail behind. Returns the new vector (functional shape so
/// callers can use it inside `serde_json::json!` literals).
#[must_use]
pub fn push_history_bounded(
    mut history: Vec<EvalResultSummary>,
    summary: EvalResultSummary,
) -> Vec<EvalResultSummary> {
    history.insert(0, summary);
    history.truncate(MAX_HISTORY);
    history
}

/// Condition type for the eval-specific "any case in the most recent run
/// failed its expectation" signal. `True` means drift detected; `False`
/// means the last run was clean. Distinct from the standard `Degraded`
/// condition because the controller itself may still be healthy when a
/// run reports drift.
pub const TYPE_CONFORMANCE_DRIFT: &str = "ConformanceDrift";

/// Stable reason strings for `KarsEval` conditions. Operators grep on
/// these in their alerting pipelines, so they must not change without a
/// CRD version bump.
pub mod reason {
    // Forward-compat taxonomy: a handful of reasons are not yet wired
    // by the reconciler but are part of the public API surface and
    // alerting contract. `#[allow(dead_code)]` is local-scope to this
    // module and does not relax warnings elsewhere.
    #![allow(dead_code)]

    pub const RECONCILED: &str = "Reconciled";
    pub const CORPUS_RESOLVED: &str = "CorpusResolved";
    pub const CORPUS_FETCH_FAILED: &str = "CorpusFetchFailed";
    pub const CORPUS_PARSE_FAILED: &str = "CorpusParseFailed";
    pub const CORPUS_BUILTIN_MISSING: &str = "CorpusBuiltinMissing";
    pub const SCHEDULED: &str = "Scheduled";
    pub const RUN_TRIGGERED: &str = "RunTriggered";
    pub const RUN_REPORT_READ: &str = "RunReportRead";
    pub const RUN_REPORT_PARSE_FAILED: &str = "RunReportParseFailed";
    pub const DRIFT_DETECTED: &str = "DriftDetected";
    pub const ALL_PASSED: &str = "AllPassed";
    pub const SPEC_INVALID: &str = "SpecInvalid";
    pub const TARGET_SANDBOX_MISSING: &str = "TargetSandboxMissing";
    pub const TARGET_SANDBOX_NOT_READY: &str = "TargetSandboxNotReady";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_roundtrip_builtin() {
        let spec = KarsEvalSpec {
            target_sandbox_ref: SandboxRef {
                name: "agent-001".into(),
            },
            corpus: CorpusSource {
                builtin: Some("jailbreak-baseline".into()),
                bundle_ref: None,
            },
            schedule: Some("0 */6 * * *".into()),
            fail_sandbox_on_drift: Some(true),
            display_name: Some("Daily jailbreak check".into()),
            runner_image: None,
        };
        let yaml = serde_yaml::to_string(&spec).unwrap();
        let parsed: KarsEvalSpec = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.target_sandbox_ref.name, "agent-001");
        assert_eq!(parsed.corpus.builtin.as_deref(), Some("jailbreak-baseline"));
        assert!(parsed.corpus.bundle_ref.is_none());
        assert_eq!(parsed.schedule.as_deref(), Some("0 */6 * * *"));
        assert_eq!(parsed.fail_sandbox_on_drift, Some(true));
    }

    #[test]
    fn spec_roundtrip_bundle() {
        let spec = KarsEvalSpec {
            target_sandbox_ref: SandboxRef {
                name: "agent-002".into(),
            },
            corpus: CorpusSource {
                builtin: None,
                bundle_ref: Some(OciArtifactRef {
                    registry: "myacr.azurecr.io".into(),
                    repository: "corpora/prompt-injection".into(),
                    digest: "sha256:abc123".into(),
                    artifact_type: "application/vnd.kars.eval-corpus.v1+json".into(),
                }),
            },
            schedule: None,
            fail_sandbox_on_drift: None,
            display_name: None,
            runner_image: None,
        };
        let json = serde_json::to_string(&spec).unwrap();
        let parsed: KarsEvalSpec = serde_json::from_str(&json).unwrap();
        let bundle = parsed.corpus.bundle_ref.unwrap();
        assert_eq!(bundle.digest, "sha256:abc123");
        assert_eq!(parsed.target_sandbox_ref.name, "agent-002");
    }

    #[test]
    fn history_caps_at_max() {
        let mut history: Vec<EvalResultSummary> = Vec::new();
        for i in 0..(MAX_HISTORY + 5) {
            history = push_history_bounded(
                history,
                EvalResultSummary {
                    at: format!("2026-01-01T00:{:02}:00Z", i),
                    corpus_digest: "sha256:zz".into(),
                    total: 10,
                    passed: 10,
                    failed: 0,
                    errored: 0,
                    job_name: format!("eval-{i}"),
                },
            );
        }
        assert_eq!(history.len(), MAX_HISTORY);
        // Newest entry is at index 0 (highest `i`).
        assert_eq!(history[0].job_name, format!("eval-{}", MAX_HISTORY + 4));
        // Oldest kept entry is index MAX_HISTORY-1.
        assert_eq!(history[MAX_HISTORY - 1].job_name, "eval-5");
    }

    #[test]
    fn history_newest_first_invariant() {
        let mut history = vec![];
        history = push_history_bounded(
            history,
            EvalResultSummary {
                at: "2026-01-01T00:00:00Z".into(),
                corpus_digest: "sha256:a".into(),
                total: 1,
                passed: 1,
                failed: 0,
                errored: 0,
                job_name: "first".into(),
            },
        );
        history = push_history_bounded(
            history,
            EvalResultSummary {
                at: "2026-01-02T00:00:00Z".into(),
                corpus_digest: "sha256:b".into(),
                total: 1,
                passed: 0,
                failed: 1,
                errored: 0,
                job_name: "second".into(),
            },
        );
        assert_eq!(history[0].job_name, "second");
        assert_eq!(history[1].job_name, "first");
    }

    #[test]
    fn corpus_source_default_is_jailbreak_baseline() {
        let c = CorpusSource::default();
        assert_eq!(c.builtin.as_deref(), Some("jailbreak-baseline"));
        assert!(c.bundle_ref.is_none());
    }

    #[test]
    fn annotation_constant_is_dns_subdomain() {
        assert!(ANNOTATION_RUN_NOW.contains('/'));
        let (domain, key) = ANNOTATION_RUN_NOW.split_once('/').unwrap();
        assert_eq!(domain, "kars.azure.com");
        assert_eq!(key, "run-now");
    }

    #[test]
    fn conformance_drift_type_name_is_stable() {
        // The condition type is part of the operator-facing CRD contract;
        // alerting pipelines grep on this string. Changing it requires a
        // CRD version bump.
        assert_eq!(TYPE_CONFORMANCE_DRIFT, "ConformanceDrift");
    }

    #[test]
    fn reasons_are_distinct() {
        let set = [
            reason::RECONCILED,
            reason::CORPUS_RESOLVED,
            reason::CORPUS_FETCH_FAILED,
            reason::CORPUS_PARSE_FAILED,
            reason::CORPUS_BUILTIN_MISSING,
            reason::SCHEDULED,
            reason::RUN_TRIGGERED,
            reason::RUN_REPORT_READ,
            reason::RUN_REPORT_PARSE_FAILED,
            reason::DRIFT_DETECTED,
            reason::ALL_PASSED,
            reason::SPEC_INVALID,
            reason::TARGET_SANDBOX_MISSING,
            reason::TARGET_SANDBOX_NOT_READY,
        ];
        let unique: std::collections::HashSet<&str> = set.iter().copied().collect();
        assert_eq!(set.len(), unique.len(), "duplicate reason string");
    }

    #[test]
    fn eval_result_serde_keeps_camel_case() {
        let r = EvalResult {
            schema_version: "v1".into(),
            corpus_digest: "sha256:x".into(),
            total: 10,
            passed: 7,
            failed: 3,
            errored: 0,
            corpus_label: "builtin:jailbreak-baseline".into(),
            job_name: "karseval-foo-runnow-abc".into(),
            first_failing_cases: vec!["jb-007".into(), "jb-011".into()],
        };
        let v = serde_json::to_value(&r).unwrap();
        assert!(v.get("schemaVersion").is_some());
        assert!(v.get("corpusDigest").is_some());
        assert!(v.get("firstFailingCases").is_some());
        assert!(v.get("schema_version").is_none());
    }
}
