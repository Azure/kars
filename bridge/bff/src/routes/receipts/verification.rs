// kars Bridge BFF — receipt verification, extracted without changing wire formats.

use super::*;

/// The outcome of an in-browser cryptographic verification — performed
/// server-side against the controller's public key and any configured
/// independent pins, so an auditor gets a verdict and underlying evidence
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
    /// The published anchor checked against any configured independent pins.
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
    /// Advisory witness metadata; its public key is not published in V0,
    /// so neither its identity nor its co-signature is verified here.
    pub witness_key_id: Option<String>,
    pub witness_signature_b64: Option<String>,
}

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
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
    verify_log_integrity_with_pins(log, AnchorPins::from_env())
}

pub(crate) fn verify_log_integrity_with_pins(
    log: &ReceiptLog,
    pins: Result<AnchorPins, &'static str>,
) -> LogIntegrity {
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
        let anchor = match pins.and_then(|pins| pins.resolve(log)) {
            Ok(anchor) => {
                out.anchor_pinned = anchor.pinned;
                Some(anchor)
            }
            Err(reason) => {
                tracing::warn!(reason, "Receipt checkpoint trust anchor rejected");
                None
            }
        };
        let cp_sig_ok = anchor
            .as_ref()
            .and_then(|anchor| VerifyingKey::from_bytes(&anchor.public_key).ok())
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
pub(super) fn pae(payload_type: &str, body: &[u8]) -> Vec<u8> {
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
/// trusts a key embedded in the receipt. Independently configured pins
/// constrain the cluster-published anchor when present.
pub async fn verify_receipt(
    state: State<AppState>,
    principal: Extension<Principal>,
    path: Path<(String, String)>,
) -> AppResult<Json<VerifyResult>> {
    verify_receipt_with_pins(state, principal, path, AnchorPins::from_env()).await
}

pub(crate) async fn verify_receipt_with_pins(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    pins: Result<AnchorPins, &'static str>,
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

    // 1) Both verification paths resolve the same configured trust boundary.
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
    let anchor = match pins.and_then(|pins| pins.resolve(&log)) {
        Ok(anchor) => anchor,
        Err(reason) => {
            push(
                &mut checks,
                "Trust anchor",
                false,
                reason.into(),
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
    let anchor_key_id = anchor.key_id;
    let anchor_pub_b64 = anchor.public_key_b64;
    let anchor_scheme = anchor.scheme;
    evidence.anchor_key_id = Some(anchor_key_id.clone());
    evidence.anchor_public_key_b64 = Some(anchor_pub_b64.clone());
    push(
        &mut checks,
        "Trust anchor",
        true,
        if anchor.pinned {
            "The controller's public key matches the configured out-of-band pins.".into()
        } else {
            "The signature is checked against the cluster-published key, not a key in the receipt. No independent out-of-band pin is configured.".into()
        },
        None,
        None,
    );

    // 2) Receipt key id matches the anchor.
    let key_match = spec.key_id == anchor_key_id;
    push(
        &mut checks,
        "Signing key identity",
        key_match,
        if key_match {
            "The receipt's key identifier matches the published anchor.".into()
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
    let pub_bytes = Some(anchor.public_key);
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
                        detail: "A witness co-signature is published, but its public key is unavailable here, so it is shown without verification.".into(),
                        expected: Some(w.clone()),
                        computed: None,
                    });
            } else {
                checks.push(VerifyCheck {
                    name: "Independent witness".into(),
                    passed: false,
                    advisory: true,
                    detail: "No independent witness key is published; checkpoint signature verification is unaffected.".into(),
                    expected: None,
                    computed: None,
                });
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
