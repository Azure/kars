// Copyright (c) Pal Lakatos-Toth.

use axum::Json;
use axum::extract::State;
use kube::core::DynamicObject;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::state::AppState;

use super::{created_of, name_of, ns_of, require_cluster, s, spec, status, upstream};

// ─── Audit: receipts + inclusion log + checkpoint ────────────────────────────

#[derive(Debug, Serialize)]
pub struct ReceiptSummaryDto {
    pub name: String,
    pub namespace: String,
    pub task: Option<String>,
    pub envelope_digest: Option<String>,
    pub key_id: Option<String>,
    pub inclusion_seq: Option<i64>,
    pub created: Option<String>,
    /// At-a-glance verdict from the receipt's claim matrix (`spec.claims`, which
    /// the CRD already carries) — `verified` (all required non-regulatory claims
    /// PASS), `failed` (any FAIL), `partial` (required evidence incomplete), or
    /// `none` (no claims). Regulatory maturity is advisory and shown in detail.
    pub verdict: String,
}

/// Reduce a receipt's `(class, status)` claim pairs to an overall verdict.
/// Any FAIL/ERROR ⇒ "failed". Otherwise the badge reflects the CRYPTOGRAPHIC
/// claims (integrity + conformance + completeness) — the "regulatory" claim and
/// any "OMITTED" status are advisory V0-maturity disclosures that must NOT block
/// a "verified" verdict (else every receipt reads "partial" forever). `class`
/// is expected lowercased, `status` uppercased.
fn receipt_verdict(claims: &[(String, String)]) -> &'static str {
    if claims.is_empty() {
        return "none";
    }
    if claims.iter().any(|(_, s)| s == "FAIL" || s == "ERROR") {
        return "failed";
    }
    let core: Vec<&(String, String)> = claims
        .iter()
        .filter(|(class, status)| class != "regulatory" && status != "OMITTED")
        .collect();
    if !core.is_empty() && core.iter().all(|(_, s)| s == "PASS" || s == "OK") {
        "verified"
    } else {
        "partial"
    }
}

fn to_receipt_summary(o: &DynamicObject) -> ReceiptSummaryDto {
    let sp = spec(o);
    // The regulatory claim is a V0 maturity dimension — it is ALWAYS "PARTIAL"
    // or "OMITTED" until an external KMS/transparency anchor lands (a named V1
    // follow-up), and "OMITTED" is an honest disclosure, not a verification
    // failure. Treating either as blocking meant NO receipt could ever read
    // "Verified" (every one showed "Partial"), making the verdict useless. So
    // the badge reflects the CRYPTOGRAPHIC claims (integrity + conformance +
    // completeness); the regulatory/omitted maturity is still shown in detail.
    let claims: Vec<(String, String)> = sp
        .get("claims")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    let status = c
                        .get("status")
                        .and_then(|s| s.as_str())?
                        .to_ascii_uppercase();
                    let class = c
                        .get("class")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    Some((class, status))
                })
                .collect()
        })
        .unwrap_or_default();
    let verdict = receipt_verdict(&claims).to_string();
    ReceiptSummaryDto {
        name: name_of(o),
        namespace: ns_of(o),
        task: sp
            .get("taskRef")
            .and_then(|r| r.get("name"))
            .and_then(|n| n.as_str())
            .map(|x| x.to_string()),
        envelope_digest: s(sp, "envelopeDigest"),
        key_id: s(sp, "keyId"),
        inclusion_seq: status(o).get("inclusionSeq").and_then(|x| x.as_i64()),
        created: created_of(o),
        verdict,
    }
}

#[derive(Debug, Serialize)]
pub struct AuditDto {
    pub receipts: Vec<ReceiptSummaryDto>,
    pub inclusion_log_size: i64,
    pub checkpoint: Option<CheckpointSummaryDto>,
    /// Real cryptographic integrity verdict — the whole hash chain recomputed
    /// and the signed checkpoint verified against the published anchor. Drives
    /// the audit banner so it reflects verification, not field presence.
    pub integrity: crate::routes::receipts::LogIntegrity,
}

#[derive(Debug, Serialize)]
pub struct CheckpointSummaryDto {
    pub tree_size: i64,
    pub root_hash: String,
    pub key_id: String,
    pub published_at: Option<String>,
}

/// `GET /api/operator/audit` — the audit substrate: every governance receipt,
/// the inclusion-log size, and the signed checkpoint (signed tree head).
pub async fn get_audit(State(state): State<AppState>) -> AppResult<Json<AuditDto>> {
    let cluster = require_cluster(&state)?;
    let items = cluster
        .list_kind_all("KarsReceipt")
        .await
        .map_err(upstream)?;
    let mut receipts: Vec<ReceiptSummaryDto> = items.iter().map(to_receipt_summary).collect();
    receipts.sort_by_key(|a| a.inclusion_seq);

    let log = cluster
        .receipt_log()
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?;
    let inclusion_log_size = log.entries.len() as i64;

    let checkpoint = log.checkpoint.as_ref().and_then(|d| {
        let tree_size = d.get("treeSize")?.parse::<i64>().ok()?;
        Some(CheckpointSummaryDto {
            tree_size,
            root_hash: d.get("rootHash").cloned().unwrap_or_default(),
            key_id: d.get("keyId").cloned().unwrap_or_default(),
            published_at: d.get("publishedAt").cloned(),
        })
    });

    Ok(Json(AuditDto {
        receipts,
        inclusion_log_size,
        checkpoint,
        integrity: crate::routes::receipts::verify_log_integrity(&log),
    }))
}

// ─── datapath-completeness witness (optional eBPF) ───────────────────────────
//
// An independent, kernel-level attestation of what sandboxes ACTUALLY send on
// the network, cross-checked against the controller-declared egress allowlist.
// Produced out-of-band by the optional Inspektor Gadget witness
// (deploy/ebpf-witness/) and published to the `kars-datapath-witness` ConfigMap
// in kars-system. The Bridge only READS that ConfigMap — no eBPF/gadget
// dependency here. Absent ConfigMap => witness not enabled (honest empty), never
// an error.

#[derive(Serialize, Deserialize, Default)]
pub struct DatapathWitnessSandbox {
    pub namespace: String,
    pub sandbox: String,
    #[serde(default)]
    pub declared_hosts: Vec<String>,
    #[serde(default)]
    pub observed_dns: Vec<String>,
    #[serde(default)]
    pub observed_connects: u64,
    #[serde(default)]
    pub beyond_declared: Vec<String>,
    #[serde(default)]
    pub unused_declared: Vec<String>,
    pub verdict: String,
}

#[derive(Serialize)]
pub struct DatapathWitnessDto {
    /// True once the optional eBPF witness is installed and has published a
    /// verdict. False => not enabled (the web layer shows enable instructions).
    pub enabled: bool,
    pub generated_at: Option<String>,
    pub window_seconds: Option<u32>,
    pub sandboxes: Vec<DatapathWitnessSandbox>,
    /// How to turn the witness on — surfaced verbatim in the not-enabled state.
    pub install_hint: String,
}

#[derive(Deserialize)]
struct WitnessDoc {
    generated_at: Option<String>,
    window_seconds: Option<u32>,
    #[serde(default)]
    sandboxes: Vec<DatapathWitnessSandbox>,
}

pub async fn datapath_witness(
    State(state): State<AppState>,
) -> AppResult<Json<DatapathWitnessDto>> {
    let cluster = require_cluster(&state)?;
    let hint = "Enable the optional eBPF datapath witness on the cluster: \
                KARS_EBPF_WITNESS=1 deploy/ebpf-witness/install.sh --continuous"
        .to_string();

    let not_enabled = || DatapathWitnessDto {
        enabled: false,
        generated_at: None,
        window_seconds: None,
        sandboxes: Vec::new(),
        install_hint: hint.clone(),
    };

    let Some(body) = cluster
        .configmap_data("kars-datapath-witness")
        .await
        .and_then(|d| d.get("witness.json").cloned())
    else {
        return Ok(Json(not_enabled()));
    };

    match serde_json::from_str::<WitnessDoc>(&body) {
        Ok(doc) => Ok(Json(DatapathWitnessDto {
            enabled: true,
            generated_at: doc.generated_at,
            window_seconds: doc.window_seconds,
            sandboxes: doc.sandboxes,
            install_hint: hint,
        })),
        // Malformed payload is treated as not-enabled rather than a hard error —
        // the console must never 500 on optional-feature data.
        Err(_) => Ok(Json(not_enabled())),
    }
}

#[cfg(test)]
mod tests {
    use super::receipt_verdict;

    fn claim(class: &str, status: &str) -> (String, String) {
        (class.to_string(), status.to_string())
    }

    #[test]
    fn receipt_verdict_regulatory_and_omitted_are_advisory() {
        // The real V0 shape: crypto claims PASS, regulatory OMITTED. Must be
        // "verified" (regression: it used to read "partial" for every receipt).
        let v0 = vec![
            claim("integrity", "PASS"),
            claim("conformance", "PASS"),
            claim("completeness", "PASS"),
            claim("regulatory", "OMITTED"),
        ];
        assert_eq!(receipt_verdict(&v0), "verified");

        // Regulatory PARTIAL is likewise advisory.
        let v0b = vec![
            claim("integrity", "PASS"),
            claim("conformance", "PASS"),
            claim("completeness", "PASS"),
            claim("regulatory", "PARTIAL"),
        ];
        assert_eq!(receipt_verdict(&v0b), "verified");

        // A genuinely partial CORE claim (completeness) is still "partial".
        let partial = vec![
            claim("integrity", "PASS"),
            claim("conformance", "PASS"),
            claim("completeness", "PARTIAL"),
            claim("regulatory", "OMITTED"),
        ];
        assert_eq!(receipt_verdict(&partial), "partial");

        // Any FAIL/ERROR anywhere is "failed".
        let failed = vec![claim("integrity", "FAIL"), claim("conformance", "PASS")];
        assert_eq!(receipt_verdict(&failed), "failed");

        // No claims ⇒ "none".
        assert_eq!(receipt_verdict(&[]), "none");
    }

    #[test]
    fn witness_doc_parses_real_aggregator_payload() {
        // The exact shape the aggregator publishes into kars-datapath-witness.
        let body = r#"{
          "generated_at": "2026-07-02T13:51:32Z",
          "window_seconds": 15,
          "gadget": "inspektor-gadget",
          "sandboxes": [
            {"namespace":"kars-demo","sandbox":"demo",
             "declared_hosts":["api.github.com"],
             "observed_dns":["api.github.com","example.com"],
             "observed_connects":4,
             "beyond_declared":["example.com"],
             "unused_declared":[],
             "verdict":"BEYOND-DECLARED"}
          ]
        }"#;
        let doc: super::WitnessDoc = serde_json::from_str(body).expect("parse");
        assert_eq!(doc.generated_at.as_deref(), Some("2026-07-02T13:51:32Z"));
        assert_eq!(doc.window_seconds, Some(15));
        assert_eq!(doc.sandboxes.len(), 1);
        let s = &doc.sandboxes[0];
        assert_eq!(s.sandbox, "demo");
        assert_eq!(s.verdict, "BEYOND-DECLARED");
        assert_eq!(s.beyond_declared, vec!["example.com"]);
        assert_eq!(s.observed_connects, 4);
    }

    #[test]
    fn witness_sandbox_tolerates_missing_optional_arrays() {
        // Defaults must hold so a partial payload never fails deserialization.
        let s: super::DatapathWitnessSandbox =
            serde_json::from_str(r#"{"namespace":"n","sandbox":"x","verdict":"LEARN"}"#)
                .expect("parse");
        assert_eq!(s.verdict, "LEARN");
        assert!(s.declared_hosts.is_empty());
        assert!(s.observed_dns.is_empty());
        assert_eq!(s.observed_connects, 0);
    }
}
