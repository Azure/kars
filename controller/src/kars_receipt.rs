// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `KarsReceipt` CRD + Governance Receipt model (kars Bridge V0, Inc 3).
//!
//! A **Governance Receipt** is a signed, independently-verifiable record that
//! a `KarsTask` was governed under a specific trust envelope. It is the
//! "auditor's moment": a third party can take the receipt, the public-key
//! anchor, and `kars receipt verify`, and confirm — without trusting the
//! Bridge UI — what authority a task ran under and that the governance
//! invariants held.
//!
//! ## What V0 proves (and what it honestly does not)
//!
//! The receipt is an [in-toto Statement] wrapped in a [DSSE] envelope and
//! signed by the controller (see [`crate::providers::signing`]). Its claim
//! matrix is deliberately explicit so the receipt never overstates assurance:
//!
//! | class        | V0 status | meaning |
//! |--------------|-----------|---------|
//! | `integrity`  | `PASS`    | DSSE/Ed25519 signature binds the payload to the envelope digest. |
//! | `conformance`| `PASS`    | Envelope validated; any delegation strictly attenuated its parent. |
//! | `completeness`| `PARTIAL`| Covers *governance* facts (envelope, lineage, launch decision). The runtime token/cost audit chain is emitted by the inference router and is **not yet** bound in — that is the V1 upgrade. |
//! | `regulatory` | `OMITTED` | No external transparency-log / KMS anchor in V0 local signing. |
//!
//! These statuses are written verbatim into the receipt predicate *and*
//! surfaced at `spec.claims` for `kubectl`/Bridge, so the honesty travels
//! with the artifact.
//!
//! ## Determinism
//!
//! The signed Statement carries **no timestamp** and is built only from the
//! task spec + governed status. Combined with Ed25519's deterministic
//! signatures, this makes emission idempotent and lets a verifier re-derive
//! the exact Statement from the live `KarsTask` and confirm it matches
//! byte-for-byte before checking the signature. Issuance time lives in
//! `status.issuedAt` (unsigned, informational).
//!
//! [in-toto Statement]: https://github.com/in-toto/attestation/blob/main/spec/v1/statement.md
//! [DSSE]: https://github.com/secure-systems-lab/dsse

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::kars_task::{KarsTask, KarsTaskStatus};
use crate::mcp_server::LocalObjectRef;
use crate::providers::signing::{DsseEnvelope, SIGNING_SCHEME};

/// in-toto Statement type URI.
pub const STATEMENT_TYPE: &str = "https://in-toto.io/Statement/v1";
/// kars Governance Receipt predicate type URI (V0).
pub const PREDICATE_TYPE: &str = "https://kars.azure.com/attestations/GovernanceReceipt/v0";

/// `KarsReceipt.spec` — the persisted, signed Governance Receipt for one
/// `KarsTask`. The controller is the sole writer; it owns the object via an
/// owner reference to the task, so the receipt is garbage-collected with it.
#[derive(CustomResource, Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[kube(
    group = "kars.azure.com",
    version = "v1alpha1",
    kind = "KarsReceipt",
    namespaced,
    status = "KarsReceiptStatus",
    shortname = "crcpt",
    printcolumn = r#"{"name":"Task","type":"string","jsonPath":".spec.taskRef.name"}"#,
    printcolumn = r#"{"name":"EnvelopeDigest","type":"string","jsonPath":".spec.envelopeDigest"}"#,
    printcolumn = r#"{"name":"KeyId","type":"string","jsonPath":".spec.keyId"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct KarsReceiptSpec {
    /// The `KarsTask` this receipt attests, in the same namespace.
    pub task_ref: LocalObjectRef,

    /// `sha256:` digest of the trust envelope the task ran under. Mirrors the
    /// task's `status.envelopeDigest` and is bound into the signed subject.
    pub envelope_digest: String,

    /// in-toto predicate type URI — always [`PREDICATE_TYPE`] for V0.
    pub predicate_type: String,

    /// Signing scheme, e.g. `DSSEv1+ed25519`.
    pub scheme: String,

    /// Hex SHA-256 fingerprint of the signing public key. A verifier matches
    /// this against the out-of-band trust anchor, never the reverse.
    pub key_id: String,

    /// The DSSE envelope: base64 in-toto Statement + Ed25519 signature(s).
    pub dsse: DsseEnvelope,

    /// The claim matrix, surfaced for `kubectl`/Bridge without base64-decoding
    /// the payload. This is a copy of `predicate.claims`; the signed source of
    /// truth is inside `dsse.payload`.
    pub claims: Vec<Claim>,
}

/// One claim-class assertion in the receipt. `class`/`status` are constrained
/// to the small vocabularies below; kept as strings for forward-compatible
/// wire stability.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Claim {
    /// One of: `integrity`, `conformance`, `completeness`, `regulatory`.
    pub class: String,
    /// One of: `PASS`, `PARTIAL`, `FAIL`, `OMITTED`.
    pub status: String,
    /// Human-readable justification, surfaced verbatim to the auditor.
    pub detail: String,
}

impl Claim {
    fn new(class: &str, status: &str, detail: impl Into<String>) -> Self {
        Self {
            class: class.to_string(),
            status: status.to_string(),
            detail: detail.into(),
        }
    }
}

/// `KarsReceipt.status` — informational echo. The receipt's authority comes
/// from its signature, not from this block.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KarsReceiptStatus {
    /// RFC3339 issuance time (unsigned — not part of the attested payload).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued_at: Option<String>,

    /// The task `metadata.generation` this receipt was minted from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_task_generation: Option<i64>,
}

// ─────────────────────────────────────────────────────────────────────
// in-toto Statement model (the signed payload)
// ─────────────────────────────────────────────────────────────────────

/// An in-toto Statement carrying the Governance Receipt predicate. Serialized
/// to canonical JSON and signed; struct field order is the canonical order.
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Statement {
    #[serde(rename = "_type")]
    pub typ: String,
    pub subject: Vec<Subject>,
    pub predicate_type: String,
    pub predicate: Predicate,
}

/// The artifact the receipt is about: the governed task, bound to its
/// envelope digest.
#[derive(Debug, Serialize, Clone)]
pub struct Subject {
    pub name: String,
    pub digest: SubjectDigest,
}

/// Subject digest. kars truncates the envelope SHA-256 to 16 bytes for
/// compact status; the verifier compares the same truncated form.
#[derive(Debug, Serialize, Clone)]
pub struct SubjectDigest {
    /// 32-hex-char (16-byte) truncated SHA-256 of the trust envelope.
    pub sha256: String,
}

/// The Governance Receipt predicate.
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Predicate {
    pub task: PredicateTask,
    pub envelope: PredicateEnvelope,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lineage: Vec<String>,
    pub delegation: PredicateDelegation,
    pub execution: PredicateExecution,
    /// The human decisions (HITL approvals) recorded for this task — every
    /// steer is itself part of the signed record. Empty when none were taken.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub approvals: Vec<PredicateApproval>,
    pub conformance: PredicateConformance,
    pub claims: Vec<Claim>,
    pub issuer: PredicateIssuer,
}

/// One human decision bound into the receipt. Built from a decided
/// `KarsApproval` (Approved or Denied).
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PredicateApproval {
    pub name: String,
    pub action_kind: String,
    pub summary: String,
    /// `approve` or `deny`.
    pub verdict: String,
    pub decider: String,
    pub decided_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_tier: Option<i32>,
}

#[derive(Debug, Serialize, Clone)]
pub struct PredicateTask {
    pub namespace: String,
    pub name: String,
    pub objective: String,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PredicateEnvelope {
    pub tier: i32,
    pub authority_ceiling: i32,
    pub delegation_depth: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_policy_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub egress_allowlist_ref: Option<String>,
    pub digest: String,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PredicateDelegation {
    pub is_child: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_ref: Option<String>,
    pub depth_from_root: usize,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PredicateExecution {
    pub launched: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_ref: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PredicateConformance {
    /// Whether the trust envelope passed validation (always true for an
    /// emitted receipt — degraded tasks get no receipt).
    pub envelope_valid: bool,
    /// `Some(true)` if this is a child whose envelope strictly attenuated its
    /// parent's; `None` for a root task with no delegation to check.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attenuates_parent: Option<bool>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PredicateIssuer {
    pub component: String,
    pub key_id: String,
    pub scheme: String,
}

/// Build the in-toto Statement for a governed task. Pure and deterministic —
/// no timestamps, no I/O — so it is unit-testable and re-derivable by a
/// verifier.
///
/// `key_id` is the controller's signing fingerprint (bound into the issuer).
/// Returns `None` when the task is not governance-`Ready` (no envelope digest),
/// because a receipt must never bind to authority that did not validate.
pub fn build_statement(
    task: &KarsTask,
    status: &KarsTaskStatus,
    key_id: &str,
    approvals: &[PredicateApproval],
) -> Option<Statement> {
    let digest = status.envelope_digest.clone()?;
    let namespace = task
        .metadata
        .namespace
        .clone()
        .unwrap_or_else(|| "default".to_string());
    let name = task.metadata.name.clone().unwrap_or_default();
    let env = &task.spec.envelope;

    let is_child = task.spec.parent_ref.is_some();
    let attenuates_parent = is_child.then_some(true);
    let parent_ref = task.spec.parent_ref.as_ref().map(|p| p.name.clone());

    let launched = task
        .spec
        .execution
        .as_ref()
        .map(|e| e.launch)
        .unwrap_or(false);

    // The honest claim matrix (see module docs). conformance is PASS because a
    // receipt is only emitted for a validated, attenuating task.
    let conformance_detail = if is_child {
        "Trust envelope validated; delegation strictly attenuates parent authority on every axis (controller-enforced)."
    } else {
        "Trust envelope validated; root task with no delegation to attenuate."
    };
    let claims = vec![
        Claim::new(
            "integrity",
            "PASS",
            "DSSE/Ed25519 signature binds this payload to the trust-envelope digest.",
        ),
        Claim::new("conformance", "PASS", conformance_detail),
        Claim::new(
            "completeness",
            "PARTIAL",
            "Covers governance facts (envelope, lineage, launch decision). The runtime token/cost audit chain emitted by the inference router is not yet bound into this receipt (V1).",
        ),
        Claim::new(
            "regulatory",
            "OMITTED",
            "V0 uses local controller signing. No external transparency-log or KMS anchor yet (V1).",
        ),
    ];

    let predicate = Predicate {
        task: PredicateTask {
            namespace: namespace.clone(),
            name: name.clone(),
            objective: task.spec.objective.clone(),
        },
        envelope: PredicateEnvelope {
            tier: env.tier,
            authority_ceiling: env.authority_ceiling,
            delegation_depth: env.delegation_depth,
            tool_policy_ref: env.tool_policy_ref.as_ref().map(|r| r.name.clone()),
            egress_allowlist_ref: env.egress_allowlist_ref.as_ref().map(|r| r.name.clone()),
            digest: digest.clone(),
        },
        lineage: status.lineage.clone(),
        delegation: PredicateDelegation {
            is_child,
            parent_ref,
            depth_from_root: status.lineage.len(),
        },
        execution: PredicateExecution {
            launched,
            phase: status.execution_phase.clone(),
            sandbox_ref: status.sandbox_ref.as_ref().map(|r| r.name.clone()),
        },
        approvals: approvals.to_vec(),
        conformance: PredicateConformance {
            envelope_valid: true,
            attenuates_parent,
        },
        claims: claims.clone(),
        issuer: PredicateIssuer {
            component: "kars-controller".to_string(),
            key_id: key_id.to_string(),
            scheme: SIGNING_SCHEME.to_string(),
        },
    };

    Some(Statement {
        typ: STATEMENT_TYPE.to_string(),
        subject: vec![Subject {
            name: format!("{namespace}/{name}"),
            // Bind to the same truncated SHA-256 the envelope digest carries,
            // stripping the `sha256:` algorithm prefix for the in-toto field.
            digest: SubjectDigest {
                sha256: digest
                    .strip_prefix("sha256:")
                    .unwrap_or(&digest)
                    .to_string(),
            },
        }],
        predicate_type: PREDICATE_TYPE.to_string(),
        predicate,
    })
}

/// Canonical JSON bytes for signing. serde serializes struct fields in
/// declaration order, so this is stable across processes.
pub fn canonical_json(statement: &Statement) -> Vec<u8> {
    serde_json::to_vec(statement).expect("Statement always serializes")
}

/// Assemble a [`KarsReceiptSpec`] from a signed envelope + statement.
pub fn build_spec(
    task_name: &str,
    envelope_digest: &str,
    key_id: &str,
    dsse: DsseEnvelope,
    claims: Vec<Claim>,
) -> KarsReceiptSpec {
    KarsReceiptSpec {
        task_ref: LocalObjectRef {
            name: task_name.to_string(),
        },
        envelope_digest: envelope_digest.to_string(),
        predicate_type: PREDICATE_TYPE.to_string(),
        scheme: SIGNING_SCHEME.to_string(),
        key_id: key_id.to_string(),
        dsse,
        claims,
    }
}

/// Convert decided `KarsApproval`s for a task into deterministic receipt
/// facts. Only **Approved** or **Denied** approvals (a real human decision)
/// are included; Pending/Expired/Stale ones are not part of the attested
/// human-decision record. Sorted by name so the signed payload is stable.
pub fn approval_facts(approvals: &[crate::kars_approval::KarsApproval]) -> Vec<PredicateApproval> {
    use crate::kars_approval::{PHASE_APPROVED, PHASE_DENIED};
    use kube::ResourceExt;

    let mut facts: Vec<PredicateApproval> = approvals
        .iter()
        .filter_map(|a| {
            let status = a.status.as_ref()?;
            let phase = status.phase.as_deref()?;
            let verdict = match phase {
                PHASE_APPROVED => "approve",
                PHASE_DENIED => "deny",
                _ => return None,
            };
            Some(PredicateApproval {
                name: a.name_any(),
                action_kind: a.spec.action.kind.clone(),
                summary: a.spec.action.summary.clone(),
                verdict: verdict.to_string(),
                decider: status.decider.clone().unwrap_or_default(),
                decided_at: status.decided_at.clone().unwrap_or_default(),
                requested_tier: a.spec.action.requested_tier,
            })
        })
        .collect();
    facts.sort_by(|a, b| a.name.cmp(&b.name));
    facts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kars_task::{KarsTaskSpec, TaskEnvelope, TaskExecution};

    fn ready_task(child: bool) -> (KarsTask, KarsTaskStatus) {
        let mut spec = KarsTaskSpec {
            objective: "do the thing".to_string(),
            envelope: TaskEnvelope {
                tier: 3,
                authority_ceiling: 2,
                delegation_depth: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        if child {
            spec.parent_ref = Some(LocalObjectRef {
                name: "parent".to_string(),
            });
        }
        spec.execution = Some(TaskExecution {
            launch: true,
            runtime: None,
        });
        let mut task = KarsTask::new("demo", spec);
        task.metadata.namespace = Some("kars-system".to_string());
        let status = KarsTaskStatus {
            phase: Some("Ready".to_string()),
            envelope_digest: Some("sha256:deadbeefdeadbeefdeadbeefdeadbeef".to_string()),
            lineage: if child {
                vec!["root".to_string(), "parent".to_string()]
            } else {
                vec![]
            },
            execution_phase: Some("Degraded".to_string()),
            ..Default::default()
        };
        (task, status)
    }

    #[test]
    fn no_receipt_without_digest() {
        let (task, mut status) = ready_task(false);
        status.envelope_digest = None;
        assert!(build_statement(&task, &status, "kid", &[]).is_none());
    }

    #[test]
    fn root_statement_shape() {
        let (task, status) = ready_task(false);
        let st = build_statement(&task, &status, "kid123", &[]).unwrap();
        assert_eq!(st.typ, STATEMENT_TYPE);
        assert_eq!(st.predicate_type, PREDICATE_TYPE);
        assert_eq!(st.subject[0].name, "kars-system/demo");
        // sha256: prefix stripped for the in-toto digest field.
        assert_eq!(st.subject[0].digest.sha256, "deadbeefdeadbeefdeadbeefdeadbeef");
        assert!(!st.predicate.delegation.is_child);
        assert_eq!(st.predicate.conformance.attenuates_parent, None);
        assert_eq!(st.predicate.issuer.key_id, "kid123");
    }

    #[test]
    fn child_statement_records_attenuation_and_lineage() {
        let (task, status) = ready_task(true);
        let st = build_statement(&task, &status, "kid", &[]).unwrap();
        assert!(st.predicate.delegation.is_child);
        assert_eq!(st.predicate.delegation.parent_ref.as_deref(), Some("parent"));
        assert_eq!(st.predicate.delegation.depth_from_root, 2);
        assert_eq!(st.predicate.conformance.attenuates_parent, Some(true));
        assert_eq!(st.predicate.lineage, vec!["root", "parent"]);
    }

    #[test]
    fn claim_matrix_is_honest() {
        let (task, status) = ready_task(false);
        let st = build_statement(&task, &status, "kid", &[]).unwrap();
        let by = |c: &str| {
            st.predicate
                .claims
                .iter()
                .find(|x| x.class == c)
                .unwrap()
                .status
                .clone()
        };
        assert_eq!(by("integrity"), "PASS");
        assert_eq!(by("conformance"), "PASS");
        assert_eq!(by("completeness"), "PARTIAL");
        assert_eq!(by("regulatory"), "OMITTED");
    }

    #[test]
    fn canonical_json_is_stable() {
        let (task, status) = ready_task(true);
        let a = canonical_json(&build_statement(&task, &status, "kid", &[]).unwrap());
        let b = canonical_json(&build_statement(&task, &status, "kid", &[]).unwrap());
        assert_eq!(a, b);
        // Sanity: it really is the in-toto envelope.
        let s = String::from_utf8(a).unwrap();
        assert!(s.contains("\"_type\":\"https://in-toto.io/Statement/v1\""));
        assert!(s.contains("\"predicateType\""));
    }

    #[test]
    fn launched_execution_is_recorded() {
        let (task, status) = ready_task(false);
        let st = build_statement(&task, &status, "kid", &[]).unwrap();
        assert!(st.predicate.execution.launched);
        assert_eq!(st.predicate.execution.phase.as_deref(), Some("Degraded"));
    }

    #[test]
    fn approvals_are_bound_into_the_predicate() {
        let (task, status) = ready_task(false);
        let approvals = vec![PredicateApproval {
            name: "raise-tier".to_string(),
            action_kind: "tierRaise".to_string(),
            summary: "raise to tier 4 for the migration".to_string(),
            verdict: "approve".to_string(),
            decider: "alice@example.com".to_string(),
            decided_at: "2026-06-26T10:00:00+00:00".to_string(),
            requested_tier: Some(4),
        }];
        let st = build_statement(&task, &status, "kid", &approvals).unwrap();
        assert_eq!(st.predicate.approvals.len(), 1);
        assert_eq!(st.predicate.approvals[0].verdict, "approve");
        assert_eq!(st.predicate.approvals[0].requested_tier, Some(4));
        // The signed payload carries the human decision.
        let json = String::from_utf8(canonical_json(&st)).unwrap();
        assert!(json.contains("\"approvals\""));
        assert!(json.contains("alice@example.com"));
    }

    #[test]
    fn approval_facts_filters_to_decided_and_sorts() {
        use crate::kars_approval::{
            ApprovalAction, KarsApproval, KarsApprovalSpec, KarsApprovalStatus,
        };
        let mk = |name: &str, phase: Option<&str>, decider: Option<&str>| {
            let mut a = KarsApproval::new(
                name,
                KarsApprovalSpec {
                    task_ref: LocalObjectRef {
                        name: "t".to_string(),
                    },
                    action: ApprovalAction {
                        kind: "checkpoint".to_string(),
                        summary: "ok?".to_string(),
                        ..Default::default()
                    },
                    ttl: None,
                    decision: None,
                },
            );
            a.status = Some(KarsApprovalStatus {
                phase: phase.map(|s| s.to_string()),
                decider: decider.map(|s| s.to_string()),
                decided_at: decider.map(|_| "2026-06-26T10:00:00+00:00".to_string()),
                ..Default::default()
            });
            a
        };
        let approvals = vec![
            mk("zebra", Some("Approved"), Some("z")),
            mk("pending-one", Some("Pending"), None),
            mk("alpha", Some("Denied"), Some("a")),
            mk("stale-one", Some("Stale"), None),
        ];
        let facts = approval_facts(&approvals);
        // Only the two decided ones, sorted by name.
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].name, "alpha");
        assert_eq!(facts[0].verdict, "deny");
        assert_eq!(facts[1].name, "zebra");
        assert_eq!(facts[1].verdict, "approve");
    }
}
