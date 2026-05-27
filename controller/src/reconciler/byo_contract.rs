// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! BYO (Bring-Your-Own runtime) contract validation.
//!
//! Phase 3 S8 closes the audit gap "BYO contract verification is
//! warn-only; ship strict-mode admission".
//!
//! ## Two layers of validation
//!
//! 1. **CR-level** (this module). Enforces what is checkable from the
//!    `KarsSandbox.spec.runtime.byo` fields without registry I/O:
//!    - `contract_version` is non-empty AND in the supported set.
//!    - `image` is non-empty AND shape-valid (contains a tag or digest).
//!
//! 2. **Registry-level** (out of scope for this slice). The image's
//!    `org.kars.runtime.contract` label must equal the declared
//!    `contract_version`. That requires an authenticated registry pull
//!    in the controller's hot path, which is a substantial new
//!    dependency surface (rate limits, auth flow, image-cache); left
//!    for Phase 4. CR-level enforcement *alone* prevents the most
//!    common operator error (typoing the contract version) and gives
//!    operators a stable failure mode to match against in CI.
//!
//! ## Strict vs. warn-only
//!
//! - `byo_strict = false` (default): violations are returned as
//!   `ContractIssue` with severity `Warn`; the reconciler stamps a
//!   condition but proceeds with the Deployment. This preserves Phase
//!   2 behaviour bit-for-bit.
//!
//! - `byo_strict = true`: violations escalate to `Severity::Strict`;
//!   the reconciler short-circuits with `Degraded=True`,
//!   `Reason=BYOContractInvalid`, and does **not** create the Deployment.
//!   No partial state is materialised — fail-closed.

use crate::crd::ByoRuntimeConfig;

/// Set of contract versions the controller knows how to mount.
///
/// Bumping this is a deliberate operator action — adding a value here
/// means we have wired the corresponding mount/env conventions in
/// the deployment builder. The audit guidance "no scaffolding, no
/// half-done pieces" applies: the value `v1` is real and the only
/// version implemented today.
pub const SUPPORTED_BYO_CONTRACT_VERSIONS: &[&str] = &["v1", "1", "1.0"];

/// Severity tag on a contract issue.
///
/// `Warn` is reported via the `RuntimeReady` condition (`status=True
/// reason=BYOContractAdvisory`), the Deployment is created.
/// `Strict` is reported via the `Degraded` condition (`status=True
/// reason=BYOContractInvalid`), the Deployment is NOT created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warn,
    Strict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractIssue {
    pub severity: Severity,
    pub field: &'static str,
    pub message: String,
}

/// Validate a [`ByoRuntimeConfig`] against the BYO contract.
///
/// Returns the list of violations in declaration order. The caller
/// chooses how to surface them based on `byo_strict`.
pub fn validate(cfg: &ByoRuntimeConfig, byo_strict: bool) -> Vec<ContractIssue> {
    let mut issues = Vec::new();
    let escalate = |issue: ContractIssue| -> ContractIssue {
        if byo_strict {
            ContractIssue {
                severity: Severity::Strict,
                ..issue
            }
        } else {
            issue
        }
    };

    // 1. contract_version
    if cfg.contract_version.trim().is_empty() {
        issues.push(escalate(ContractIssue {
            severity: Severity::Warn,
            field: "byo.contractVersion",
            message: format!(
                "byo.contractVersion is required; declare one of {:?}",
                SUPPORTED_BYO_CONTRACT_VERSIONS
            ),
        }));
    } else if !SUPPORTED_BYO_CONTRACT_VERSIONS.contains(&cfg.contract_version.as_str()) {
        issues.push(escalate(ContractIssue {
            severity: Severity::Warn,
            field: "byo.contractVersion",
            message: format!(
                "byo.contractVersion=`{}` is not in the supported set {:?}",
                cfg.contract_version, SUPPORTED_BYO_CONTRACT_VERSIONS
            ),
        }));
    }

    // 2. image shape
    if cfg.image.trim().is_empty() {
        issues.push(escalate(ContractIssue {
            severity: Severity::Warn,
            field: "byo.image",
            message: "byo.image is required and must be a fully-qualified image reference".into(),
        }));
    } else if !looks_like_image_reference(&cfg.image) {
        issues.push(escalate(ContractIssue {
            severity: Severity::Warn,
            field: "byo.image",
            message: format!(
                "byo.image=`{}` does not look like a valid image reference (expected `repo:tag` or `repo@sha256:...`)",
                cfg.image
            ),
        }));
    }

    issues
}

/// Return `true` for refs of the shape `host/path:tag` or
/// `host/path@sha256:abc...`. Deliberately permissive — the goal is
/// to catch operator typos (forgotten tag, leading whitespace), not
/// to re-implement the OCI distribution-spec parser.
fn looks_like_image_reference(s: &str) -> bool {
    let trimmed = s.trim();
    if trimmed.len() != s.len() || trimmed.is_empty() {
        return false;
    }
    if trimmed.contains(' ') || trimmed.contains('\t') || trimmed.contains('\n') {
        return false;
    }
    // Either ":<tag>" (after the last '/') or "@sha256:..." somewhere.
    if let Some(at_idx) = trimmed.find("@sha256:") {
        // Must have at least one alphanumeric character after.
        return trimmed.len() > at_idx + "@sha256:".len() + 8;
    }
    let last_slash = trimmed.rfind('/').unwrap_or(0);
    let tail = &trimmed[last_slash..];
    if let Some(colon_idx) = tail.find(':') {
        let after = &tail[colon_idx + 1..];
        return !after.is_empty() && !after.starts_with('/');
    }
    false
}

/// Return the highest severity in the issue list, or `None` for a
/// clean validation. Useful for the reconciler's branch logic.
pub fn worst_severity(issues: &[ContractIssue]) -> Option<Severity> {
    let mut worst: Option<Severity> = None;
    for i in issues {
        match (worst, i.severity) {
            (None, s) => worst = Some(s),
            (Some(Severity::Warn), Severity::Strict) => worst = Some(Severity::Strict),
            _ => {}
        }
    }
    worst
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(image: &str, contract_version: &str) -> ByoRuntimeConfig {
        ByoRuntimeConfig {
            image: image.into(),
            command: None,
            args: None,
            env: None,
            contract_version: contract_version.into(),
        }
    }

    #[test]
    fn happy_path_yields_no_issues() {
        let issues = validate(&cfg("ghcr.io/foo/bar:v1.2.3", "v1"), true);
        assert!(issues.is_empty(), "expected no issues, got {issues:?}");
    }

    #[test]
    fn digest_pinning_is_accepted() {
        let issues = validate(
            &cfg(
                "ghcr.io/foo/bar@sha256:1111111111111111111111111111111111111111111111111111111111111111",
                "v1",
            ),
            true,
        );
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn unknown_contract_version_warns_in_loose_mode() {
        let issues = validate(&cfg("ghcr.io/foo/bar:1", "v999"), false);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, Severity::Warn);
        assert_eq!(issues[0].field, "byo.contractVersion");
    }

    #[test]
    fn unknown_contract_version_fails_strict_in_strict_mode() {
        let issues = validate(&cfg("ghcr.io/foo/bar:1", "v999"), true);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, Severity::Strict);
    }

    #[test]
    fn missing_tag_is_a_violation() {
        let issues = validate(&cfg("ghcr.io/foo/bar", "v1"), true);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].field, "byo.image");
    }

    #[test]
    fn empty_fields_are_violations() {
        let issues = validate(&cfg("", ""), false);
        assert_eq!(issues.len(), 2);
    }

    #[test]
    fn whitespace_in_image_rejected() {
        let issues = validate(&cfg(" ghcr.io/foo/bar:v1", "v1"), true);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].field, "byo.image");
    }

    #[test]
    fn worst_severity_picks_strict_over_warn() {
        let mut issues = vec![ContractIssue {
            severity: Severity::Warn,
            field: "x",
            message: "a".into(),
        }];
        assert_eq!(worst_severity(&issues), Some(Severity::Warn));
        issues.push(ContractIssue {
            severity: Severity::Strict,
            field: "y",
            message: "b".into(),
        });
        assert_eq!(worst_severity(&issues), Some(Severity::Strict));
    }

    #[test]
    fn empty_issues_return_none_severity() {
        assert_eq!(worst_severity(&[]), None);
    }
}
