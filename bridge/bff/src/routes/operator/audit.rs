// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Copyright (c) Pal Lakatos-Toth.

use axum::Json;
use axum::extract::State;
use kube::core::DynamicObject;
use serde::Serialize;

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
/// This summarizes recorded claims, not cryptographic verification. All three
/// required classes must be present and passing; only regulatory maturity is
/// advisory. `class` is expected lowercased, `status` uppercased.
fn receipt_verdict(claims: &[(String, String)]) -> &'static str {
    if claims.is_empty() {
        return "none";
    }
    if claims.iter().any(|(_, s)| s == "FAIL" || s == "ERROR") {
        return "failed";
    }
    let core: Vec<&(String, String)> = claims
        .iter()
        .filter(|(class, _)| class != "regulatory")
        .collect();
    let required_present = ["integrity", "conformance", "completeness"]
        .iter()
        .all(|required| core.iter().any(|(class, _)| class == required));
    if required_present && core.iter().all(|(_, s)| s == "PASS" || s == "OK") {
        "verified"
    } else {
        "partial"
    }
}

fn to_receipt_summary(o: &DynamicObject) -> ReceiptSummaryDto {
    let sp = spec(o);
    let claims: Vec<(String, String)> = sp
        .get("claims")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .map(|c| {
                    let status = c
                        .get("status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_ascii_uppercase();
                    let class = c
                        .get("class")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    (class, status)
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

#[path = "datapath.rs"]
mod datapath;
pub use datapath::datapath_witness;

#[cfg(test)]
mod tests {
    use super::receipt_verdict;

    fn claim(class: &str, status: &str) -> (String, String) {
        (class.to_string(), status.to_string())
    }

    #[test]
    fn receipt_verdict_regulatory_maturity_is_advisory() {
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
    fn receipt_verdict_requires_every_core_claim() {
        let classes = ["integrity", "conformance", "completeness"];
        for missing in classes {
            let claims: Vec<_> = classes
                .iter()
                .filter(|class| **class != missing)
                .map(|class| claim(class, "PASS"))
                .chain([claim("regulatory", "PASS")])
                .collect();
            assert_eq!(receipt_verdict(&claims), "partial", "{missing}");
        }
        assert_eq!(receipt_verdict(&[claim("integrity", "PASS")]), "partial");
        assert_eq!(receipt_verdict(&[claim("unknown", "PASS")]), "partial");
    }

    #[test]
    fn receipt_verdict_never_discards_incomplete_core_claims() {
        let classes = ["integrity", "conformance", "completeness"];
        for incomplete in classes {
            for status in ["OMITTED", "PARTIAL", "", "UNKNOWN"] {
                let claims: Vec<_> = classes
                    .iter()
                    .map(|class| claim(class, if *class == incomplete { status } else { "OK" }))
                    .collect();
                assert_eq!(receipt_verdict(&claims), "partial", "{incomplete}: {status}");
            }
        }
        let mut claims: Vec<_> = classes.iter().map(|class| claim(class, "OK")).collect();
        assert_eq!(receipt_verdict(&claims), "verified");
        claims.push(claim("completeness", "OMITTED"));
        assert_eq!(receipt_verdict(&claims), "partial");
        claims.push(claim("regulatory", "ERROR"));
        assert_eq!(receipt_verdict(&claims), "failed");
    }

    #[test]
    fn receipt_summary_preserves_incomplete_claims_and_normalizes_case() {
        let mut object: kube::core::DynamicObject = serde_json::from_value(serde_json::json!({
            "apiVersion": "kars.io/v1alpha1",
            "kind": "KarsReceipt",
            "metadata": {"name": "receipt-run", "namespace": "test"},
            "spec": {"claims": [
                {"class": "INTEGRITY", "status": "pass"},
                {"class": "conformance", "status": "ok"},
                {"class": "completeness", "status": "PASS"}
            ]}
        }))
        .unwrap();
        assert_eq!(super::to_receipt_summary(&object).verdict, "verified");
        object.data["spec"]["claims"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"class": "completeness"}));
        assert_eq!(super::to_receipt_summary(&object).verdict, "partial");
    }
}
