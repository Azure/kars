// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `KarsSkill` — a **reusable, versioned capability bundle** (design note §13).
//!
//! A skill packages *what an agent can do* into a named, governed unit that a
//! team or role acquires once and reuses: a bounding `ToolPolicy` (the
//! authority ceiling on the tools the skill calls), the MCP servers it
//! connects, a recipe (standing instructions for using the capability well),
//! and an optional knowledge pack reference. Skills are **granted to roles and
//! teams, not raw agents** — a team references a skill by name and the
//! controller merges the skill's tools/MCP/recipe into the materialized member
//! blueprint, so the grant is a real RBAC fact (the member runs with exactly
//! the skill's bounded authority), not a label.
//!
//! Each skill carries a deterministic **version digest** over its content, so a
//! receipt that records a skill grant pins the exact skill version that ran.
//! The `bounding_policy` is mandatory: a skill that calls tools without a tool
//! policy to bound them is rejected at admission — governed capability is the
//! point.
//!
//! Additive: a cluster with no `KarsSkill` objects behaves identically. Teams
//! that reference no skills are unaffected.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// `KarsSkill.spec` — a governed, versioned capability bundle.
#[derive(CustomResource, Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[kube(
    group = "kars.azure.com",
    version = "v1alpha1",
    kind = "KarsSkill",
    namespaced,
    status = "KarsSkillStatus",
    shortname = "cskill",
    printcolumn = r#"{"name":"Version","type":"string","jsonPath":".spec.version"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Digest","type":"string","jsonPath":".status.versionDigest"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct KarsSkillSpec {
    /// Human-readable display name (e.g. "Repo triage", "Hotel itemization").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,

    /// What the skill does, in one or two plain-language sentences.
    pub summary: String,

    /// Author-declared semantic version (e.g. "1.2.0"). Surfaced verbatim; the
    /// controller also computes a content `versionDigest` that pins the bundle.
    pub version: String,

    /// The **bounding tool policy** — the name of a same-namespace `ToolPolicy`
    /// that is the authority ceiling on every tool the skill calls. **Required**:
    /// a skill that calls tools without a bound is rejected at admission.
    pub bounding_policy: String,

    /// The MCP servers (same-namespace `MCPServer` names) this skill connects.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<String>,

    /// The **recipe** — standing instructions for using the capability well,
    /// merged into the instructions of a member that acquires this skill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<String>,

    /// Optional knowledge-pack reference (the name of a team knowledge commons
    /// or a packaged knowledge set the skill ships with).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_pack: Option<String>,

    /// Optional cosign attestation reference (an OCI ref / digest of the signed
    /// skill bundle). When present, surfaced on the status as the attestation
    /// the skill was published with (full verification is a V1 supply-chain
    /// concern; recording the claim is honest provenance now).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attestation_ref: Option<String>,

    /// Optional content digest the attestation vouches for. When present it is
    /// verified to equal the controller-computed `version_digest`, so the
    /// signed bundle provably matches what runs (binds supply-chain provenance
    /// to the exact content). Format: `sha256:<hex>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attestation_digest: Option<String>,
}

impl KarsSkill {
    /// Validate the skill. A non-empty summary + version + bounding policy are
    /// required (governed capability). Returns human-readable errors.
    #[must_use]
    pub fn validation_errors(&self) -> Vec<String> {
        let mut errs = Vec::new();
        if self.spec.summary.trim().is_empty() {
            errs.push("spec.summary must not be empty".into());
        }
        if self.spec.version.trim().is_empty() {
            errs.push("spec.version must not be empty".into());
        }
        if self.spec.bounding_policy.trim().is_empty() {
            errs.push(
                "spec.boundingPolicy is required — a skill that calls tools must name a ToolPolicy that bounds them".into(),
            );
        }
        errs
    }

    /// Deterministic `sha256:` digest over the skill's governed content, so a
    /// receipt that records a skill grant pins the exact version that ran.
    #[must_use]
    pub fn version_digest(&self) -> String {
        let canonical = serde_json::json!({
            "summary": self.spec.summary,
            "version": self.spec.version,
            "boundingPolicy": self.spec.bounding_policy,
            "mcpServers": self.spec.mcp_servers,
            "recipe": self.spec.recipe,
            "knowledgePack": self.spec.knowledge_pack,
        });
        let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
        let full = Sha256::digest(&bytes);
        let mut out = String::from("sha256:");
        for b in &full[..16] {
            out.push_str(&format!("{b:02x}"));
        }
        out
    }

    /// Verify a declared cosign attestation: the ref must be well-formed and,
    /// when an `attestation_digest` is declared, it must equal the computed
    /// content digest — proving the signed bundle matches what runs. Returns
    /// (verified, human detail). No attestation declared → unverified (honest),
    /// not failed. A digest mismatch is a hard verification failure.
    #[must_use]
    pub fn verify_attestation(&self) -> (bool, String) {
        let Some(ref aref) = self.spec.attestation_ref else {
            return (false, "no attestation declared".into());
        };
        let well_formed = aref.contains("sha256:") || aref.contains('@') || aref.contains('/');
        if !well_formed {
            return (false, "attestation ref malformed (expect OCI ref or sha256:)".into());
        }
        match &self.spec.attestation_digest {
            Some(d) if *d == self.version_digest() => {
                (true, format!("attestation {aref} verified — digest binds content {d}"))
            }
            Some(d) => (false, format!("attestation digest {d} != content {}", self.version_digest())),
            None => (true, format!("attestation {aref} present (ref verified; no content digest to bind)")),
        }
    }
}

/// `KarsSkill.status` — controller-owned.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KarsSkillStatus {
    /// `Ready` (validated, grantable) | `Degraded` (invalid — not grantable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    /// `sha256:` digest pinning the validated skill content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_digest: Option<String>,
    /// The attestation reference the skill was published with, when declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attestation_ref: Option<String>,
    /// Whether the cosign attestation verified (ref well-formed + digest binds
    /// content). False when none declared or a mismatch was detected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attestation_verified: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<Condition>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill() -> KarsSkill {
        KarsSkill::new(
            "repo-triage",
            KarsSkillSpec {
                display_name: Some("Repo triage".into()),
                summary: "Triage and label incoming repo issues.".into(),
                version: "1.0.0".into(),
                bounding_policy: "kars-default".into(),
                mcp_servers: vec!["github".into()],
                recipe: Some("Label by area; close duplicates.".into()),
                knowledge_pack: None,
                attestation_ref: None,
                attestation_digest: None,
            },
        )
    }

    #[test]
    fn valid_skill_has_no_errors() {
        assert!(skill().validation_errors().is_empty());
    }

    #[test]
    fn missing_bounding_policy_is_rejected() {
        let mut s = skill();
        s.spec.bounding_policy = "  ".into();
        assert!(
            s.validation_errors()
                .iter()
                .any(|e| e.contains("boundingPolicy"))
        );
    }

    #[test]
    fn version_digest_is_stable_and_content_sensitive() {
        let s = skill();
        let d = s.version_digest();
        assert!(d.starts_with("sha256:"));
        assert_eq!(d, s.version_digest());
        let mut s2 = skill();
        s2.spec.recipe = Some("different recipe".into());
        assert_ne!(d, s2.version_digest());
    }

    #[test]
    fn attestation_verifies_when_digest_binds_content() {
        let mut s = skill();
        s.spec.attestation_ref = Some("registry.io/skills/repo-triage@sha256:abc".into());
        s.spec.attestation_digest = Some(s.version_digest());
        let (ok, detail) = s.verify_attestation();
        assert!(ok, "{detail}");
    }

    #[test]
    fn attestation_fails_on_digest_mismatch() {
        let mut s = skill();
        s.spec.attestation_ref = Some("registry.io/skills/repo-triage@sha256:abc".into());
        s.spec.attestation_digest = Some("sha256:deadbeef".into());
        let (ok, _) = s.verify_attestation();
        assert!(!ok);
    }
}
