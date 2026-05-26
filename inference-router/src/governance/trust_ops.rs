// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Trust-management operations on `Governance`.
//!
//! Extracted from `governance/mod.rs` per plan §4.2 hotspot
//! decomposition. These methods all delegate to AGT's
//! `TrustManager` (shipped in the `agentmesh` crate) for the
//! authoritative trust-score store; the logic here is the
//! Kars-side wrapping (clamping rules, audit-event emission,
//! metrics counters, response-shape JSON).
//!
//! Pure refactor — function bodies are byte-identical to the
//! originals; only their containing file changed.

use std::sync::atomic::Ordering;

use serde_json::Value;

use super::{Governance, tier_label};
use crate::metrics;

impl Governance {
    // ── Trust ────────────────────────────────────────────────────────────

    /// Update trust score with clamped semantics.
    ///
    /// Matches server.py: ±200 delta per update, max 500 for new agents,
    /// self-trust rejection, clamped to 0–1000.
    pub fn update_trust(
        &self,
        agent_id: &str,
        requested_score: u32,
        _interactions: u64,
    ) -> Result<Value, &'static str> {
        // Reject self-trust updates
        if agent_id == self.sandbox_name {
            return Err("Cannot update own trust score");
        }

        let existing = self.trust.get_trust_score(agent_id);
        let old_score = existing.score;
        let is_new = existing.interactions == 0;

        if is_new {
            // Phase F2 — TrustGraph bootstrap.
            //
            // For brand-new peers (zero AGT interactions), opportunistically
            // consult the controller-published TrustGraph projection for a
            // signed edge `sandbox_name → agent_id`. If one exists and is
            // not expired, use its score (still subject to the AGT 500 cap
            // applied below) instead of the caller-supplied requested_score.
            //
            // Hard invariants:
            //   • Never overrides an existing AGT score (interactions > 0).
            //   • Never exceeds the AGT bootstrap cap of 500 (matches the
            //     existing `requested_score.min(500)` semantics).
            //   • Self-edges are pre-filtered by the projection lookup —
            //     defence in depth against a tampered ConfigMap.
            //   • An empty / absent projection is a no-op (identical to
            //     pre-F2 behaviour).
            let now_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let bootstrap_score = self
                .trust_graph
                .direct_edge(&self.sandbox_name, agent_id, now_unix)
                .map(|e| e.score);

            let initial = bootstrap_score.unwrap_or(requested_score).min(500);

            self.trust.set_trust(agent_id, initial);
            self.trust.record_success(agent_id);

            if let Some(score) = bootstrap_score {
                metrics::AGT_TRUSTGRAPH_BOOTSTRAPS.inc();
                tracing::info!(
                    peer = agent_id,
                    bootstrap_score = score,
                    capped_initial = initial,
                    sandbox = %self.sandbox_name,
                    projection_version = %self.trust_graph.version_hash(),
                    "AGT trust bootstrapped from TrustGraph projection edge"
                );
                self.audit_log(
                    agent_id,
                    &format!("trustgraph_bootstrap:{}", score),
                    "success",
                );
            }
        } else if requested_score >= old_score {
            // Positive interaction — use SDK's built-in reward + decay + interaction bump.
            self.trust.record_success(agent_id);
        } else {
            // Negative interaction — use SDK's built-in penalty + decay + interaction bump.
            self.trust.record_failure(agent_id);
        }

        let updated = self.trust.get_trust_score(agent_id);
        metrics::AGT_KNOWN_AGENTS.set(self.trust.all_agents().len() as i64);

        // Record last-interaction timestamp for operator UX (SDK TrustScore
        // doesn't expose this).
        if let Ok(mut map) = self.peer_last_seen.lock() {
            map.insert(agent_id.to_string(), std::time::SystemTime::now());
        }

        self.audit
            .log(agent_id, &format!("trust_update:{}", agent_id), "success");

        Ok(serde_json::json!({
            "ok": true,
            "agent_id": agent_id,
            "score": updated.score,
            "interactions": updated.interactions,
        }))
    }

    /// Get all trust scores with tier labels (matching API JSON shape).
    pub fn all_trust_scores(&self) -> Vec<Value> {
        let last_seen = self
            .peer_last_seen
            .lock()
            .ok()
            .map(|m| m.clone())
            .unwrap_or_default();
        self.trust
            .all_agents()
            .into_iter()
            .map(|ts| {
                let last_iso = last_seen
                    .get(&ts.agent_id)
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| {
                        // Emit epoch-seconds with 'Z' suffix — operator.ts
                        // parser recognises /^\d+Z$/ and converts to ms.
                        format!("{}Z", d.as_secs())
                    })
                    .unwrap_or_default();
                serde_json::json!({
                    "agent_id": ts.agent_id,
                    "score": ts.score,
                    "tier": tier_label(ts.score),
                    "interactions": ts.interactions,
                    "last_interaction": last_iso,
                })
            })
            .collect()
    }

    /// Get trust score for a single agent (matching API JSON shape).
    #[allow(dead_code)] // Used by individual trust route
    pub fn get_trust_score_json(&self, agent_id: &str) -> Value {
        let ts = self.trust.get_trust_score(agent_id);
        serde_json::json!({
            "agent_id": ts.agent_id,
            "score": ts.score,
            "tier": tier_label(ts.score),
            "interactions": ts.interactions,
            "last_interaction": "",
        })
    }

    /// Delete trust state for an agent by resetting to initial score (0 interactions).
    /// The SDK TrustManager has no delete method, so we set score to 0.
    pub fn delete_trust(&self, agent_id: &str) -> Value {
        self.trust.set_trust(agent_id, 0);
        self.audit_log(agent_id, "trust_delete", "success");
        metrics::AGT_KNOWN_AGENTS.set(self.trust.all_agents().len() as i64);
        serde_json::json!({
            "ok": true,
            "agent_id": agent_id,
            "deleted": true,
        })
    }

    // ── Content flag ─────────────────────────────────────────────────────

    /// Report content safety flag and optionally penalize trust.
    pub fn report_content_flag(
        &self,
        agent_id: &str,
        _flags: &Value,
        filtered: &[String],
        detected: &[String],
        penalty: i32,
    ) -> Value {
        self.metrics.content_flags.fetch_add(1, Ordering::Relaxed);

        // Prometheus: increment per-category counters
        for cat in filtered.iter().chain(detected.iter()) {
            metrics::AGT_CONTENT_FLAGS.with_label_values(&[cat]).inc();
        }
        if filtered.is_empty() && detected.is_empty() {
            metrics::AGT_CONTENT_FLAGS
                .with_label_values(&["unknown"])
                .inc();
        }

        let flag_summary: String = filtered
            .iter()
            .chain(detected.iter())
            .cloned()
            .collect::<Vec<_>>()
            .join(",");

        self.audit_log(
            agent_id,
            &format!("content_flag:{}", flag_summary),
            "flagged",
        );

        self.behavior.record(agent_id, false);

        if penalty < 0 {
            let existing = self.trust.get_trust_score(agent_id);
            let old_score = existing.score;
            let new_score = old_score.saturating_sub((-penalty) as u32);
            self.trust.set_trust(agent_id, new_score);

            tracing::warn!(
                agent_id,
                categories = %flag_summary,
                penalty,
                old_score,
                new_score,
                "Content flag with trust penalty"
            );

            return serde_json::json!({
                "ok": true,
                "penalty_applied": penalty,
                "trust_score": new_score,
                "previous_score": old_score,
            });
        }

        serde_json::json!({
            "ok": true,
            "penalty_applied": 0,
            "trust_score": null,
        })
    }
}
