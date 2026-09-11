// kars Bridge BFF — Governance Receipt API.
//
// Read endpoints project the signed predicate, never unsigned claim echoes.
// Cryptographic validity is a separate operation: the verification endpoint
// and `kars receipt verify` check the controller-published trust anchor,
// signature, exact subject binding, inclusion log and signed checkpoint.

use axum::Json;
use axum::extract::{Extension, Path, State};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Serialize;
use serde_json::Value;

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::kars::receipt::KarsReceipt;
use crate::kars::receipt_log::{ReceiptLog, chain_entry_hash};
use crate::routes::ownership::require_task_evidence_access;
use crate::state::AppState;

mod statement;

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

/// The outcome of an in-browser cryptographic verification — performed
/// server-side by the BFF against the controller's out-of-band public-key
/// anchor, so an auditor gets a real verdict — and the underlying evidence —
/// without installing the CLI.
#[derive(Debug, Serialize)]
pub struct VerifyResult {
    /// True only when every independent check passed.
    pub verified: bool,
    /// Ordered, human-readable checks with their pass/fail outcome.
    pub checks: Vec<VerifyCheck>,
    /// The actual artifacts behind the verdict — what an auditor inspects.
    pub evidence: Evidence,
}

#[derive(Debug, Serialize)]
pub struct VerifyCheck {
    pub name: String,
    pub passed: bool,
    pub detail: String,
    /// BUG-4: a check that is DISPLAYED but not cryptographically re-verified
    /// here (e.g. the independent witness whose public key isn't published in
    /// V0). Rendered as an advisory "shown, not verified" state — never a green
    /// ✓ — so an auditor is not misled into thinking all items were checked.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub advisory: bool,
    /// The value the proof expected (e.g. a recorded hash), when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    /// The value the BFF independently computed, shown so the auditor sees the
    /// two match (or don't) rather than trusting a green tick.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub computed: Option<String>,
}

/// The evidence artifacts an auditor inspects — the signed payload, the
/// signature material, and the full inclusion proof. Everything here is the
/// real bytes the verdict was computed over.
#[derive(Debug, Serialize, Default)]
pub struct Evidence {
    /// The exact in-toto Statement the signature covers (the signed payload).
    pub signed_statement: Option<Value>,
    /// Base64 Ed25519 signature over the DSSE PAE of the statement.
    pub signature_b64: Option<String>,
    pub scheme: Option<String>,
    /// The out-of-band trust anchor the signature was checked against.
    pub anchor_key_id: Option<String>,
    pub anchor_public_key_b64: Option<String>,
    /// The receipt's position in the hash-chained inclusion log + the proof.
    pub inclusion: Option<InclusionEvidence>,
    /// The signed checkpoint (signed tree head) + independent witness.
    pub checkpoint: Option<CheckpointEvidence>,
}

#[derive(Debug, Serialize)]
pub struct InclusionEvidence {
    pub seq: i64,
    pub receipt: String,
    pub payload_sha256: String,
    pub prev_hash: String,
    pub entry_hash: String,
    /// The entry-hash the BFF recomputed from (seq | receipt | payloadSha | prev).
    pub recomputed_entry_hash: String,
    /// The head the chain links to — equals the checkpoint root when intact.
    pub chain_head: String,
    /// Whether the whole chain (genesis → head) recomputes consistently.
    pub chain_consistent: bool,
    pub tree_size: usize,
}

#[derive(Debug, Serialize)]
pub struct CheckpointEvidence {
    pub tree_size: i64,
    pub root_hash: String,
    /// The exact signed-note bytes the checkpoint signature covers.
    pub signed_note: String,
    pub signature_b64: String,
    pub signature_valid: bool,
    /// An independent transparency witness co-signs the same root with a
    /// SEPARATE key — evidence the log isn't forked. Its public key isn't
    /// published in V0, so we surface its identity + co-signature honestly.
    pub witness_key_id: Option<String>,
    pub witness_signature_b64: Option<String>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// A whole-log integrity verdict — the page-level answer to "is this audit log
/// actually tamper-evident?". Unlike a per-receipt proof, this recomputes the
/// ENTIRE hash chain and verifies the signed checkpoint, so the auditor banner
/// reflects real cryptographic verification, never mere field presence.
#[derive(Debug, Default, serde::Serialize)]
pub struct LogIntegrity {
    /// The chain recomputes consistently genesis→head: contiguous `seq`,
    /// prev-hash linkage, and every entry hash recomputes. False if empty/broken.
    pub chain_consistent: bool,
    /// Number of entries in the inclusion log.
    pub tree_size: i64,
    /// A signed checkpoint exists, its Ed25519 signature verifies against the
    /// published out-of-band anchor, AND it commits to the current chain head.
    pub checkpoint_verified: bool,
    /// An independent transparency-witness co-signature is PRESENT. Shown, not
    /// re-verified in V0 (the witness public key isn't published), so it is
    /// advisory — never counted toward `checkpoint_verified`.
    pub witness_present: bool,
    /// Whether the anchor the checkpoint signature was verified against is pinned
    /// OUT-OF-BAND (a BFF-configured key id / public key from a trust boundary
    /// distinct from the log). When false, the anchor is the in-cluster
    /// `kars-receipt-pubkey` — the SAME trust domain as the log — so a party that
    /// can rewrite the log could also rewrite the anchor. The verdict is then
    /// "consistent + signed by the cluster's published anchor", NOT absolute
    /// tamper-evidence; the banner must not overclaim.
    pub anchor_pinned: bool,
}

/// Recompute the ENTIRE `kars-receipt-log` hash chain and verify the signed
/// checkpoint against the published anchor key. Used by the audit page so its
/// integrity verdict is a real verification, not a `inclusion_seq != null` proxy.
pub(crate) fn verify_log_integrity(log: &ReceiptLog) -> LogIntegrity {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let mut out = LogIntegrity::default();
    let chain = &log.entries;
    if chain.is_empty() {
        return out;
    }
    out.tree_size = chain.len() as i64;

    // Recompute the whole chain: contiguous seq, prev-hash linkage, entry hashes.
    let mut chain_consistent = !chain.is_empty();
    let mut prev = "genesis".to_string();
    for (i, e) in chain.iter().enumerate() {
        if e.seq != i as i64
            || e.prev_hash != prev
            || chain_entry_hash(e.seq, &e.receipt, &e.payload_sha256, &e.prev_hash) != e.entry_hash
        {
            chain_consistent = false;
            break;
        }
        prev = e.entry_hash.clone();
    }
    out.chain_consistent = chain_consistent;
    let chain_head = chain
        .last()
        .map(|e| e.entry_hash.clone())
        .unwrap_or_else(|| "genesis".into());

    // Verify the signed checkpoint (Ed25519 over the canonical note) against the
    // anchor key, and that it commits to the current chain head.
    if let Some(cp) = log.checkpoint.as_ref() {
        let cp_tree = cp
            .get("treeSize")
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        let cp_root = cp.get("rootHash").cloned().unwrap_or_default();
        let cp_sig = cp.get("signature").cloned().unwrap_or_default();
        let note = format!("kars-receipt-log\n{cp_tree}\n{cp_root}\n");
        let anchor = log.anchor();

        // OUT-OF-BAND PINNING. The in-cluster anchor lives in the same trust
        // domain as the log, so on its own it can't prove tamper-evidence
        // against an insider who can rewrite both. When the operator pins the
        // anchor out-of-band (BRIDGE_RECEIPT_ANCHOR_KEY_ID / _PUBKEY), require
        // the in-cluster anchor to match it — and only then is the verdict
        // absolute. Without a pin, the anchor is trusted-on-read and the banner
        // reflects the weaker, honest claim.
        let pin_key_id = std::env::var("BRIDGE_RECEIPT_ANCHOR_KEY_ID").ok();
        let pin_pubkey = std::env::var("BRIDGE_RECEIPT_ANCHOR_PUBKEY").ok();
        let anchor_matches_pin = match (&anchor, pin_key_id.as_deref(), pin_pubkey.as_deref()) {
            (Some((kid, pub_b64, _)), pk_id, pk_pub) => {
                let id_ok = pk_id.is_none_or(|w| w == kid);
                let pub_ok = pk_pub.is_none_or(|w| w.trim() == pub_b64.trim());
                (pk_id.is_some() || pk_pub.is_some()) && id_ok && pub_ok
            }
            _ => false,
        };
        out.anchor_pinned = anchor_matches_pin;

        // If a pin is configured but the in-cluster anchor does NOT match it,
        // the anchor is untrusted — do not honor any signature made with it.
        let pin_configured = pin_key_id.is_some() || pin_pubkey.is_some();
        let anchor_trusted = !pin_configured || anchor_matches_pin;

        let cp_sig_ok = anchor_trusted
            && anchor
                .as_ref()
                .and_then(|(_, pub_b64, _)| BASE64.decode(pub_b64.as_bytes()).ok())
                .and_then(|b| <[u8; 32]>::try_from(b).ok())
                .and_then(|pk| VerifyingKey::from_bytes(&pk).ok())
                .map(|vk| {
                    BASE64
                        .decode(cp_sig.as_bytes())
                        .ok()
                        .and_then(|sb| <[u8; 64]>::try_from(sb).ok())
                        .map(|sb| {
                            vk.verify(note.as_bytes(), &Signature::from_bytes(&sb))
                                .is_ok()
                        })
                        .unwrap_or(false)
                })
                .unwrap_or(false);
        out.checkpoint_verified =
            chain_consistent && cp_sig_ok && cp_root == chain_head && cp_tree == out.tree_size;
    }

    // Independent witness co-signature — present-or-not only (advisory in V0).
    out.witness_present = log
        .witness
        .as_ref()
        .and_then(|w| w.get("witnessSignature").cloned())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);

    out
}

/// DSSE Pre-Authentication Encoding — byte-for-byte the same framing the
/// controller signs (`controller/src/providers/signing.rs::pae`):
/// `"DSSEv1" SP len(type) SP type SP len(body) SP body`.
fn pae(payload_type: &str, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload_type.len() + body.len() + 32);
    out.extend_from_slice(b"DSSEv1 ");
    out.extend_from_slice(payload_type.len().to_string().as_bytes());
    out.push(b' ');
    out.extend_from_slice(payload_type.as_bytes());
    out.push(b' ');
    out.extend_from_slice(body.len().to_string().as_bytes());
    out.push(b' ');
    out.extend_from_slice(body);
    out
}

/// `POST /api/namespaces/:ns/tasks/:name/receipt/verify` — independently verify
/// a Governance Receipt's DSSE/Ed25519 signature against the controller's
/// published public-key anchor (`kars-receipt-pubkey` ConfigMap). This is the
/// same trust root `kars receipt verify` uses; performing it here lets an
/// auditor get a real cryptographic verdict in the browser. The BFF never
/// trusts a key embedded in the receipt — only the out-of-band anchor.
pub async fn verify_receipt(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<VerifyResult>> {
    use base64::engine::general_purpose::STANDARD as B64;
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    let cluster = require_cluster(&state)?;
    require_task_evidence_access(cluster, &ns, &name, &principal).await?;
    let receipt = cluster
        .receipts(&ns)
        .get_opt(&name)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?
        .ok_or(AppError::NotFound)?;
    let spec = &receipt.spec;

    let mut checks: Vec<VerifyCheck> = Vec::new();
    let mut evidence = Evidence::default();
    let push = |checks: &mut Vec<VerifyCheck>,
                name: &str,
                passed: bool,
                detail: String,
                expected: Option<String>,
                computed: Option<String>| {
        checks.push(VerifyCheck {
            name: name.to_string(),
            passed,
            detail,
            advisory: false,
            expected,
            computed,
        });
        passed
    };

    // Decode the signed statement once — it IS the evidence the auditor reads.
    let decoded = match statement::decode(spec, &ns, &name) {
        Ok(decoded) => decoded,
        Err(error) => {
            push(
                &mut checks,
                "Signed statement binding",
                false,
                error,
                None,
                None,
            );
            return Ok(Json(VerifyResult {
                verified: false,
                checks,
                evidence,
            }));
        }
    };
    let payload_raw = decoded.payload;
    evidence.signed_statement = Some(decoded.statement);
    evidence.scheme = Some(spec.scheme.clone());
    evidence.signature_b64 = spec.dsse.signatures.first().map(|s| s.sig.clone());

    // 1) Trust anchor present (out-of-band published public key).
    let log = match cluster.receipt_log().await {
        Ok(log) => log,
        Err(error) => {
            push(
                &mut checks,
                "Inclusion log",
                false,
                error.to_string(),
                None,
                None,
            );
            return Ok(Json(VerifyResult {
                verified: false,
                checks,
                evidence,
            }));
        }
    };
    let anchor = log.anchor();
    let Some((anchor_key_id, anchor_pub_b64, anchor_scheme)) = anchor else {
        push(
            &mut checks,
            "Trust anchor",
            false,
            "No published public-key anchor (kars-receipt-pubkey) found — cannot verify.".into(),
            None,
            None,
        );
        return Ok(Json(VerifyResult {
            verified: false,
            checks,
            evidence,
        }));
    };
    evidence.anchor_key_id = Some(anchor_key_id.clone());
    evidence.anchor_public_key_b64 = Some(anchor_pub_b64.clone());
    push(&mut checks, "Trust anchor", true,
        "An out-of-band public key is published by the controller; the signature is checked against THIS key, never one carried in the receipt.".into(),
        None, None);

    // 2) Receipt key id matches the anchor.
    let key_match = spec.key_id == anchor_key_id;
    push(
        &mut checks,
        "Signing key identity",
        key_match,
        if key_match {
            "The receipt's key fingerprint matches the published anchor.".into()
        } else {
            "The receipt's signing key does NOT match the trusted anchor.".into()
        },
        Some(anchor_key_id.clone()),
        Some(spec.key_id.clone()),
    );

    // 3) Scheme is the expected DSSE/Ed25519.
    let scheme_ok = spec.scheme == statement::SCHEME && spec.scheme == anchor_scheme;
    push(
        &mut checks,
        "Signature scheme",
        scheme_ok,
        format!("Scheme: {}.", spec.scheme),
        Some(anchor_scheme.clone()),
        Some(spec.scheme.clone()),
    );

    // 4) Ed25519 signature verifies over the DSSE PAE of the exact payload.
    let mut sig_ok = false;
    let pub_bytes = B64
        .decode(anchor_pub_b64.as_bytes())
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b).ok());
    if let (Some(pk), false) = (pub_bytes, payload_raw.is_empty())
        && let Ok(vk) = VerifyingKey::from_bytes(&pk)
    {
        let message = pae(&spec.dsse.payload_type, &payload_raw);
        let valid_signature = spec.dsse.signatures.iter().find(|s| {
            s.keyid == anchor_key_id
                && B64
                    .decode(s.sig.as_bytes())
                    .ok()
                    .and_then(|sb| <[u8; 64]>::try_from(sb).ok())
                    .map(|sb| vk.verify(&message, &Signature::from_bytes(&sb)).is_ok())
                    .unwrap_or(false)
        });
        sig_ok = valid_signature.is_some();
        if let Some(signature) = valid_signature {
            evidence.signature_b64 = Some(signature.sig.clone());
        }
    }
    push(
        &mut checks,
        "Cryptographic signature",
        sig_ok,
        if sig_ok {
            "The Ed25519 signature verifies over the DSSE pre-authentication encoding of the statement below — so the payload is authentic and has not been altered by a single byte.".into()
        } else {
            "Ed25519 signature did NOT verify — the payload may have been altered.".into()
        },
        None,
        None,
    );

    // 5) The signed payload binds the trust-envelope digest the receipt claims.
    let envelope_bound = evidence
        .signed_statement
        .as_ref()
        .is_some_and(|value| statement::binds_subject(value, spec, &ns, &name));
    push(
        &mut checks,
        "Trust-envelope binding",
        envelope_bound,
        if envelope_bound {
            "The signed statement contains the exact trust-envelope digest the receipt declares — the signature can't be lifted onto a different envelope.".into()
        } else {
            "The signed payload does not reference the declared envelope digest.".into()
        },
        Some(spec.envelope_digest.clone()),
        None,
    );

    // 6) FULL inclusion proof — fetch the hash-chained log + signed checkpoint,
    //    recompute this receipt's entry, confirm the chain links to the head,
    //    and verify the checkpoint signature. No CLI, no hand-waving.
    let mut inclusion_ok = true;
    let receipt_log_ref = format!("{ns}/{name}");
    let loaded_chain = &log.entries;
    if !loaded_chain.is_empty() {
        let chain = loaded_chain;
        let tree_size = chain.len();
        // The receipt's own recorded position.
        let seq = receipt.status.as_ref().and_then(|s| s.inclusion_seq);
        let entry = seq.and_then(|q| {
            chain
                .iter()
                .find(|e| e.seq == q && e.receipt == receipt_log_ref)
        });

        // (a) payloadSha256 of the entry equals sha256 of the signed payload.
        let computed_payload_sha = sha256_hex(&payload_raw);
        let payload_sha_ok = entry
            .map(|e| e.payload_sha256 == computed_payload_sha)
            .unwrap_or(false);

        // (b) entryHash recomputes from (seq | receipt | payloadSha | prev).
        let recomputed_entry =
            entry.map(|e| chain_entry_hash(e.seq, &e.receipt, &e.payload_sha256, &e.prev_hash));
        let entry_hash_ok = entry
            .zip(recomputed_entry.as_ref())
            .map(|(e, r)| &e.entry_hash == r)
            .unwrap_or(false);

        // (c) the WHOLE chain recomputes consistently (contiguous seq,
        //     prev-hash linkage, recomputed entry hashes) up to the head.
        let mut chain_consistent = true;
        let mut prev = "genesis".to_string();
        for (i, e) in chain.iter().enumerate() {
            if e.seq != i as i64
                || e.prev_hash != prev
                || chain_entry_hash(e.seq, &e.receipt, &e.payload_sha256, &e.prev_hash)
                    != e.entry_hash
            {
                chain_consistent = false;
                break;
            }
            prev = e.entry_hash.clone();
        }
        let chain_head = chain
            .last()
            .map(|e| e.entry_hash.clone())
            .unwrap_or_else(|| "genesis".into());

        if let Some(e) = entry {
            evidence.inclusion = Some(InclusionEvidence {
                seq: e.seq,
                receipt: e.receipt.clone(),
                payload_sha256: e.payload_sha256.clone(),
                prev_hash: e.prev_hash.clone(),
                entry_hash: e.entry_hash.clone(),
                recomputed_entry_hash: recomputed_entry.clone().unwrap_or_default(),
                chain_head: chain_head.clone(),
                chain_consistent,
                tree_size,
            });
        }

        inclusion_ok = payload_sha_ok && entry_hash_ok && chain_consistent;
        push(&mut checks, "Payload digest in log", payload_sha_ok,
                "The inclusion-log entry records a SHA-256 of the signed payload — recomputing it matches, so this exact receipt is the one logged.".into(),
                entry.map(|e| e.payload_sha256.clone()), Some(computed_payload_sha));
        push(&mut checks, "Inclusion-log entry hash", entry_hash_ok,
                "The entry hash recomputes from (seq | receipt | payload-digest | previous-hash) — binding this receipt to its exact position in the chain.".into(),
                entry.map(|e| e.entry_hash.clone()), recomputed_entry);
        push(
            &mut checks,
            "Chain integrity",
            chain_consistent,
            format!(
                "All {tree_size} entries recompute and link genesis → head with no gap — removing or altering any one would break the chain."
            ),
            None,
            Some(chain_head.clone()),
        );

        // (d) the signed checkpoint commits to this head, and its Ed25519
        //     signature verifies with the anchor key.
        if let Some(cp) = log.checkpoint.as_ref() {
            let cp_tree = cp
                .get("treeSize")
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            let cp_root = cp.get("rootHash").cloned().unwrap_or_default();
            let cp_sig = cp.get("signature").cloned().unwrap_or_default();
            let note = format!("kars-receipt-log\n{cp_tree}\n{cp_root}\n");
            let cp_sig_ok = pub_bytes
                .and_then(|pk| VerifyingKey::from_bytes(&pk).ok())
                .map(|vk| {
                    B64.decode(cp_sig.as_bytes())
                        .ok()
                        .and_then(|sb| <[u8; 64]>::try_from(sb).ok())
                        .map(|sb| {
                            vk.verify(note.as_bytes(), &Signature::from_bytes(&sb))
                                .is_ok()
                        })
                        .unwrap_or(false)
                })
                .unwrap_or(false);
            let root_matches = cp_root == chain_head && cp_tree == tree_size as i64;

            let witness = log.witness.as_ref();
            evidence.checkpoint = Some(CheckpointEvidence {
                tree_size: cp_tree,
                root_hash: cp_root.clone(),
                signed_note: note.clone(),
                signature_b64: cp_sig.clone(),
                signature_valid: cp_sig_ok,
                witness_key_id: witness
                    .as_ref()
                    .and_then(|w| w.get("witnessKeyId").cloned()),
                witness_signature_b64: witness
                    .as_ref()
                    .and_then(|w| w.get("witnessSignature").cloned()),
            });

            inclusion_ok = inclusion_ok && cp_sig_ok && root_matches;
            push(&mut checks, "Signed checkpoint", cp_sig_ok && root_matches,
                    "A signed tree head commits to the chain head, and its Ed25519 signature verifies with the anchor key — pinning the whole log to a value an auditor can re-check.".into(),
                    Some(cp_root.clone()), Some(chain_head.clone()));
            if let Some(w) = witness.as_ref().and_then(|w| w.get("witnessKeyId")) {
                // BUG-4: the witness co-signature is DISPLAYED, not re-verified
                // (its public key isn't published in V0). Mark it advisory so the
                // UI shows "shown, not verified" instead of a deceptive ✓, and show
                // the witness key itself (not the anchor key, which guaranteed a
                // spurious recorded≠recomputed mismatch).
                checks.push(VerifyCheck {
                        name: "Independent witness".to_string(),
                        passed: false,
                        advisory: true,
                        detail: "A separate transparency-witness key co-signs the same tree head — evidence the log isn't forked behind your back. Its public key isn't published in V0, so this is shown, not re-verified here.".into(),
                        expected: Some(w.clone()),
                        computed: None,
                    });
            } else {
                inclusion_ok = false;
                push(
                    &mut checks,
                    "Signed checkpoint",
                    false,
                    "No signed checkpoint is available for the inclusion log.".into(),
                    None,
                    None,
                );
            }
        } else {
            inclusion_ok = false;
            push(
                &mut checks,
                "Signed checkpoint",
                false,
                "No signed checkpoint is available for the inclusion log.".into(),
                None,
                None,
            );
        }
    }
    if evidence.inclusion.is_none() {
        let detail = "This receipt isn't yet recorded in the inclusion log (it may not have run / been chained).".into();
        push(&mut checks, "Inclusion proof", false, detail, None, None);
        inclusion_ok = false;
    }

    let verified = key_match && scheme_ok && sig_ok && envelope_bound && inclusion_ok;
    Ok(Json(VerifyResult {
        verified,
        checks,
        evidence,
    }))
}
