// kars Bridge BFF — Governance Receipt API.
//
// Read endpoints project the signed predicate, never unsigned claim echoes.
// Cryptographic validity is a separate operation: the verification endpoint
// and `kars receipt verify` check the controller-published trust anchor,
// signature, exact subject binding, inclusion log and signed checkpoint.

use axum::Json;
use axum::extract::{Extension, Path, State};
use serde::Serialize;
use serde_json::Value;

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::kars::receipt::KarsReceipt;
use crate::kars::receipt_log::{ReceiptLog, chain_entry_hash};
use crate::routes::ownership::require_task_evidence_access;
use crate::state::AppState;

mod anchor;
mod statement;
mod verification;

pub(crate) use anchor::AnchorPins;
#[cfg(test)]
use verification::pae;
use verification::sha256_hex;
pub(crate) use verification::verify_log_integrity;
pub use verification::{
    CheckpointEvidence, Evidence, InclusionEvidence, LogIntegrity, VerifyCheck, VerifyResult,
    verify_receipt,
};
#[cfg(test)]
pub(crate) use verification::{verify_log_integrity_with_pins, verify_receipt_with_pins};

/// A DSSE signature line, browser-facing.
#[derive(Debug, Serialize)]
pub struct SignatureDto {
    pub keyid: String,
    pub sig: String,
}

/// One claim-class assertion.
#[derive(Debug, Serialize)]
pub struct ClaimDto {
    pub class: String,
    pub status: String,
    pub detail: String,
}

/// Browser-facing Governance Receipt.
#[derive(Debug, Serialize)]
pub struct ReceiptDetailDto {
    pub name: String,
    pub namespace: String,
    pub task: String,
    pub envelope_digest: String,
    pub predicate_type: String,
    pub scheme: String,
    pub key_id: String,
    pub payload_type: String,
    pub signatures: Vec<SignatureDto>,
    pub claims: Vec<ClaimDto>,
    /// The decoded in-toto Statement (the signed payload), for the evidence
    /// view. This is exactly the bytes the signature covers.
    pub statement: Option<Value>,
    /// Issuance time (unsigned echo), if the controller stamped it.
    pub issued_at: Option<String>,
    /// Inclusion-log sequence number (cross-receipt tamper-evidence chain).
    pub inclusion_seq: Option<i64>,
    /// Inclusion-log entry hash.
    pub inclusion_entry_hash: Option<String>,
    pub inclusion_state: Option<String>,
    pub inclusion_error: Option<String>,
    pub log_segment: Option<String>,
    pub checkpoint_tree_size: Option<i64>,
    pub witnessed: Option<bool>,
    /// The log's signed checkpoint (signed tree head), when published.
    pub checkpoint: Option<CheckpointDto>,
    /// The exact command an auditor runs to verify independently.
    pub verify_command: String,
}

/// A compact view of the inclusion log's signed checkpoint.
#[derive(Debug, Serialize)]
pub struct CheckpointDto {
    pub tree_size: i64,
    pub root_hash: String,
    pub key_id: String,
    pub published_at: Option<String>,
}

fn require_cluster(state: &AppState) -> AppResult<&crate::kars::cluster::Cluster> {
    state.cluster().ok_or(AppError::ClusterUnavailable)
}

fn to_detail(ns: &str, r: &KarsReceipt) -> AppResult<ReceiptDetailDto> {
    use kube::ResourceExt;
    let name = r.name_any();
    let spec = &r.spec;
    let decoded = statement::decode(spec, ns, &name)
        .map_err(|error| AppError::Upstream(format!("Invalid receipt: {error}")))?;
    Ok(ReceiptDetailDto {
        name: name.clone(),
        namespace: ns.to_string(),
        task: spec.task_ref.name.clone(),
        envelope_digest: spec.envelope_digest.clone(),
        predicate_type: spec.predicate_type.clone(),
        scheme: spec.scheme.clone(),
        key_id: spec.key_id.clone(),
        payload_type: spec.dsse.payload_type.clone(),
        signatures: spec
            .dsse
            .signatures
            .iter()
            .map(|s| SignatureDto {
                keyid: s.keyid.clone(),
                sig: s.sig.clone(),
            })
            .collect(),
        claims: decoded
            .claims
            .iter()
            .map(|c| ClaimDto {
                class: c.class.clone(),
                status: c.status.clone(),
                detail: c.detail.clone(),
            })
            .collect(),
        statement: Some(decoded.statement),
        issued_at: r.status.as_ref().and_then(|s| s.issued_at.clone()),
        inclusion_seq: r.status.as_ref().and_then(|s| s.inclusion_seq),
        inclusion_entry_hash: r
            .status
            .as_ref()
            .and_then(|s| s.inclusion_entry_hash.clone()),
        inclusion_state: r.status.as_ref().and_then(|s| s.inclusion_state.clone()),
        inclusion_error: r.status.as_ref().and_then(|s| s.inclusion_error.clone()),
        log_segment: r.status.as_ref().and_then(|s| s.log_segment.clone()),
        checkpoint_tree_size: r.status.as_ref().and_then(|s| s.checkpoint_tree_size),
        witnessed: r.status.as_ref().and_then(|s| s.witnessed),
        checkpoint: None,
        verify_command: format!("kars receipt verify {name} -n {ns}"),
    })
}

/// `GET /api/namespaces/:ns/tasks/:name/receipt` — the Governance Receipt for a
/// task, or 404 if the task has none (e.g. it is Degraded, so no receipt was
/// emitted). A receipt's name matches its task's name.
pub async fn get_receipt(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<ReceiptDetailDto>> {
    let cluster = require_cluster(&state)?;
    require_task_evidence_access(cluster, &ns, &name, &principal).await?;
    let api = cluster.receipts(&ns);
    match api.get_opt(&name).await.map_err(|e| {
        if let kube::Error::Api(resp) = &e
            && (400..500).contains(&resp.code)
        {
            return AppError::Rejected(resp.message.clone());
        }
        AppError::Upstream(e.to_string())
    })? {
        Some(r) => {
            let mut dto = to_detail(&ns, &r)?;
            // Attach the cluster-global signed checkpoint, if published.
            let log = cluster
                .receipt_log()
                .await
                .map_err(|error| AppError::Upstream(error.to_string()))?;
            if let Some(data) = log.checkpoint.as_ref()
                && let (Some(size), Some(root)) = (data.get("treeSize"), data.get("rootHash"))
                && let Ok(tree_size) = size.parse::<i64>()
            {
                dto.checkpoint = Some(CheckpointDto {
                    tree_size,
                    root_hash: root.clone(),
                    key_id: data.get("keyId").cloned().unwrap_or_default(),
                    published_at: data.get("publishedAt").cloned(),
                });
            }
            Ok(Json(dto))
        }
        None => Err(AppError::NotFound),
    }
}

/// One mapped compliance control — a receipt claim expressed as an external
/// regulatory obligation, with the signed receipt as its evidence.
#[derive(Debug, Serialize)]
pub struct ComplianceControlDto {
    /// Stable control identifier, e.g. `EU-AI-Act-Art-12`.
    pub control_id: String,
    /// The framework this control belongs to.
    pub framework: String,
    /// Human reference, e.g. `Article 12 — Record-keeping`.
    pub reference: String,
    /// The receipt claim class that backs this control.
    pub receipt_class: String,
    /// The claim's status verbatim (PASS / PARTIAL / …) — never upgraded.
    pub status: String,
    /// The signed claim detail, carried as the control's evidence.
    pub evidence: String,
    /// True for the `regulatory` claim class: a named V0 architectural
    /// limitation (external transparency anchor is a V1 item), so it reads
    /// PARTIAL on every receipt this product issues, not just this task.
    /// Mirrors `VerifyCheck.advisory` (BUG-4) and `AuditReceiptRow`'s
    /// `isAdvisoryClaim` — same claim, same treatment, now carried
    /// through to the compliance pack instead of silently disagreeing
    /// with the receipt row's own "Verified" verdict.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub advisory: bool,
}

/// A compliance evidence pack derived from a single mission's signed Governance
/// Receipt. Every control is backed by a real, signed receipt claim; the pack
/// carries the envelope digest, signature scheme, transparency-log inclusion,
/// and the exact verify command so an auditor can independently confirm it.
#[derive(Debug, Serialize)]
pub struct CompliancePackDto {
    pub task: String,
    pub namespace: String,
    pub generated_at: String,
    pub predicate_type: String,
    pub envelope_digest: String,
    pub signature_scheme: String,
    pub key_id: String,
    pub inclusion_seq: Option<i64>,
    pub issued_at: Option<String>,
    pub verify_command: String,
    pub controls: Vec<ComplianceControlDto>,
    /// Count of controls whose backing claim is PASS.
    pub satisfied: usize,
    /// Count of controls that are non-PASS AND not `advisory` — a real gap
    /// worth an operator's attention, not a known V0 limitation.
    pub partial: usize,
    /// Count of controls marked `advisory` (the `regulatory` claim class) —
    /// shown separately so the pack doesn't contradict the receipt row's own
    /// "Verified" verdict, which already excludes this claim class.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub advisory: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Map a receipt claim class to the external regulatory controls it evidences.
/// The mapping is static and conservative: it names the obligation each signed
/// claim speaks to, and inherits the claim's status verbatim — a PARTIAL claim
/// never becomes a satisfied control. This is first-party audit data expressed
/// in the auditor's framework, not a self-assessed compliance grade.
fn controls_for_claim(class: &str, status: &str, detail: &str) -> Vec<ComplianceControlDto> {
    let refs: &[(&str, &str, &str)] = match class {
        // Tamper-evident signed logs of the governed run.
        "integrity" => &[
            (
                "EU-AI-Act-Art-12",
                "EU AI Act",
                "Article 12 — Record-keeping (automatic logging)",
            ),
            (
                "NIST-AI-RMF-MEASURE-2.7",
                "NIST AI RMF",
                "MEASURE 2.7 — Traceability & tamper-evidence",
            ),
        ],
        // The trust envelope was validated; delegation authority is bounded.
        "conformance" => &[
            (
                "EU-AI-Act-Art-9",
                "EU AI Act",
                "Article 9 — Risk-management system",
            ),
            (
                "NIST-AI-RMF-MAP-1",
                "NIST AI RMF",
                "MAP 1 — Context & authority established",
            ),
        ],
        // Completeness-floor controls (admission, egress, seccomp) were enforced.
        "completeness" => &[
            (
                "EU-AI-Act-Art-9-controls",
                "EU AI Act",
                "Article 9 — Risk controls in operation",
            ),
            (
                "EU-AI-Act-Art-14",
                "EU AI Act",
                "Article 14 — Human oversight",
            ),
            (
                "NIST-AI-RMF-MANAGE-2",
                "NIST AI RMF",
                "MANAGE 2 — Controls operational & monitored",
            ),
        ],
        // Signing + independent transparency witness / accountability posture.
        "regulatory" => &[
            (
                "EU-AI-Act-Art-13",
                "EU AI Act",
                "Article 13 — Transparency to deployers",
            ),
            (
                "NIST-AI-RMF-GOVERN-4",
                "NIST AI RMF",
                "GOVERN 4 — Accountability & documentation",
            ),
        ],
        _ => &[],
    };
    refs.iter()
        .map(|(id, fw, reference)| ComplianceControlDto {
            control_id: (*id).to_string(),
            framework: (*fw).to_string(),
            reference: (*reference).to_string(),
            receipt_class: class.to_string(),
            status: status.to_string(),
            evidence: detail.to_string(),
            advisory: class.eq_ignore_ascii_case("regulatory"),
        })
        .collect()
}

/// `GET /api/namespaces/:ns/tasks/:name/compliance` — a compliance evidence pack
/// generated from the mission's signed Governance Receipt. No competitor ships
/// this from first-party audit data: the receipt claims are mapped to EU AI Act
/// and NIST AI RMF controls, each backed by the signed envelope digest +
/// transparency-log inclusion, with the verify command for independent proof.
pub async fn compliance_pack(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<CompliancePackDto>> {
    let cluster = require_cluster(&state)?;
    require_task_evidence_access(cluster, &ns, &name, &principal).await?;
    let api = cluster.receipts(&ns);
    let Some(r) = api
        .get_opt(&name)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?
    else {
        return Err(AppError::NotFound);
    };
    let dto = to_detail(&ns, &r)?;
    let mut controls: Vec<ComplianceControlDto> = Vec::new();
    for c in &dto.claims {
        controls.extend(controls_for_claim(&c.class, &c.status, &c.detail));
    }
    let satisfied = controls
        .iter()
        .filter(|c| c.status.eq_ignore_ascii_case("PASS"))
        .count();
    let advisory = controls.iter().filter(|c| c.advisory).count();
    let partial = controls.len() - satisfied - advisory;
    Ok(Json(CompliancePackDto {
        task: dto.task,
        namespace: ns,
        generated_at: chrono::Utc::now().to_rfc3339(),
        predicate_type: dto.predicate_type,
        envelope_digest: dto.envelope_digest,
        signature_scheme: dto.scheme,
        key_id: dto.key_id,
        inclusion_seq: dto.inclusion_seq,
        issued_at: dto.issued_at,
        verify_command: dto.verify_command,
        controls,
        satisfied,
        partial,
        advisory,
    }))
}
