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

    /// Package bytes live in the same-namespace `karsskill-<name>` ConfigMap.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub package: bool,

    /// Exact relative paths. ConfigMap keys encode `/` as `__`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,

    /// Full SHA256 of the canonical encoded ConfigMap file map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_digest: Option<String>,

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
}

impl KarsSkill {
    pub fn validate_package(
        &self,
        package: &k8s_openapi::api::core::v1::ConfigMap,
    ) -> Result<(), String> {
        crate::skill_package::validate_owned_package(
            &serde_json::json!(self),
            &serde_json::json!(package),
        )?;
        let files = package
            .data
            .as_ref()
            .ok_or("Skill package has no file data")?;
        let document = files
            .get("SKILL.md")
            .ok_or("Skill package has no SKILL.md")?;
        let mut lines = document.lines();
        if lines.next() != Some("---") {
            return Err("SKILL.md must begin with YAML frontmatter".into());
        }
        let mut frontmatter = Vec::new();
        let mut closed = false;
        for line in lines {
            if line == "---" {
                closed = true;
                break;
            }
            frontmatter.push(line);
        }
        if !closed {
            return Err("SKILL.md YAML frontmatter is not closed".into());
        }
        #[derive(Deserialize)]
        struct Frontmatter {
            name: String,
            description: String,
        }
        let metadata: Frontmatter = serde_yaml::from_str(&frontmatter.join("\n"))
            .map_err(|error| format!("SKILL.md frontmatter is invalid YAML: {error}"))?;
        if Some(metadata.name.as_str()) != self.metadata.name.as_deref()
            || metadata.description.trim().is_empty()
            || metadata.description.len() > 1024
        {
            return Err("SKILL.md needs the matching skill name and a nonempty description of at most 1024 bytes".into());
        }
        Ok(())
    }

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
        if errs.is_empty()
            && let Err(error) = crate::skill_package::validate_spec(&serde_json::json!(self.spec))
        {
            errs.push(error);
        }
        errs
    }

    /// Deterministic `sha256:` digest over the skill's governed content, so a
    /// receipt that records a skill grant pins the exact version that ran.
    #[must_use]
    pub fn version_digest(&self) -> String {
        crate::skill_package::version_digest(&serde_json::json!(self.spec))
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
                package: false,
                files: vec![],
                package_digest: None,
                knowledge_pack: None,
                attestation_ref: None,
            },
        )
    }

    #[test]
    fn valid_skill_has_no_errors() {
        assert!(skill().validation_errors().is_empty());
    }

    #[test]
    fn skill_package_frontmatter_is_checked_after_owned_bytes() {
        for (document, valid) in [
            (
                "---\nname: repo-triage\ndescription: Review evidence\n---\nRead the supplied file.",
                true,
            ),
            (
                "---\nname: repo-triage\ndescription: |\n  Review local\n  evidence\nmetadata:\n  categories: [review]\n---\n",
                true,
            ),
            ("name: repo-triage\ndescription: No delimiters", false),
            ("---\nname: repo-triage\ndescription: Not closed", false),
            ("---\nname: other\ndescription: Wrong name\n---\n", false),
            ("---\nname: repo-triage\ndescription: ''\n---\n", false),
            (
                "---\nname: repo-triage\ndescription: [not, text]\n---\n",
                false,
            ),
            (
                "---\nname: repo-triage\nname: other\ndescription: Duplicate key\n---\n",
                false,
            ),
        ] {
            let mut s = skill();
            s.metadata.uid = Some("skill-uid".into());
            s.metadata.namespace = Some("work".into());
            s.spec.package = true;
            s.spec.files = vec!["SKILL.md".into()];
            let data = std::collections::BTreeMap::from([("SKILL.md".into(), document.into())]);
            s.spec.package_digest = Some(crate::skill_package::package_digest(&data).unwrap());
            let package = serde_json::from_value(serde_json::json!({
                "metadata":{"name":"karsskill-repo-triage","namespace":"work",
                    "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSkill",
                        "name":"repo-triage","uid":"skill-uid","controller":true}]},
                "data":data
            }))
            .unwrap();
            assert_eq!(s.validate_package(&package).is_ok(), valid, "{document}");
        }
    }

    #[test]
    fn skill_package_helm_fields_match_typed_schema() {
        use kube::CustomResourceExt;
        let generated = serde_json::to_value(KarsSkill::crd()).unwrap();
        let installed: serde_json::Value = serde_yaml::from_str(include_str!(
            "../../deploy/helm/kars/templates/crd-karsskill.yaml"
        ))
        .unwrap();
        let pointer = "/spec/versions/0/schema/openAPIV3Schema/properties/spec/properties";
        for field in ["package", "files", "packageDigest"] {
            assert_eq!(
                generated.pointer(pointer).unwrap()[field],
                installed.pointer(pointer).unwrap()[field],
                "{field}"
            );
        }
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
}
