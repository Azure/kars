// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Helm ↔ Rust CRD drift detection.
//!
//! Phase 1 ships `deploy/helm/kars/templates/crd.yaml` (KarsSandbox)
//! as hand-written YAML. Phase 2 adds `crd-mcpserver.yaml` (and, in S2,
//! `crd-toolpolicy.yaml`) the same way — operators that install via
//! `helm install` get the schema before the controller starts, and
//! admission policies that reference `mcpservers` / `toolpolicies`
//! plurals (Phase 1) become enforceable.
//!
//! The risk with hand-written YAML is drift: the Rust side
//! (`controller/src/mcp_server.rs` + `crd_validations.rs::mcp_server_crd`)
//! evolves while the helm template stays frozen. This module catches it
//! at `cargo test` time by parsing the helm YAML back into a
//! `CustomResourceDefinition` and comparing — after stripping
//! status and helm-specific metadata — to the Rust-derived CRD.
//!
//! ## Bootstrapping new CRD YAML
//!
//! When a new CRD lands (or this slice authors `crd-mcpserver.yaml`
//! for the first time), the recommended workflow:
//!
//! 1. Run `DUMP_MCP_CRD_YAML=1 cargo test --bin kars-controller helm_drift -- --nocapture`.
//! 2. Pipe stdout into `deploy/helm/kars/templates/crd-mcpserver.yaml`.
//! 3. Re-run `cargo test helm_drift` — passes.
//!
//! After that, the test guards against any unilateral edit on either side.

#![allow(dead_code)]

#[cfg(test)]
use crate::crd_validations::{
    a2a_agent_crd, egress_approval_crd, inference_policy_crd, kars_eval_crd, kars_memory_crd,
    mcp_server_crd, tool_policy_crd, trust_graph_crd,
};

const MCP_HELM_CRD_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../deploy/helm/kars/templates/crd-mcpserver.yaml"
);

const TOOLPOLICY_HELM_CRD_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../deploy/helm/kars/templates/crd-toolpolicy.yaml"
);

const A2AAGENT_HELM_CRD_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../deploy/helm/kars/templates/crd-a2aagent.yaml"
);

const INFERENCEPOLICY_HELM_CRD_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../deploy/helm/kars/templates/crd-inferencepolicy.yaml"
);

const CLAWMEMORY_HELM_CRD_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../deploy/helm/kars/templates/crd-karsmemory.yaml"
);

const CLAWEVAL_HELM_CRD_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../deploy/helm/kars/templates/crd-karseval.yaml"
);

const TRUSTGRAPH_HELM_CRD_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../deploy/helm/kars/templates/crd-trustgraph.yaml"
);

const EGRESSAPPROVAL_HELM_CRD_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../deploy/helm/kars/templates/crd-egressapproval.yaml"
);

/// Strip non-schema fields that legitimately differ between the Rust
/// `CustomResource::crd()` output and the helm template (helm labels,
/// status block, metadata.creationTimestamp, etc.). The comparison key
/// is the spec + selected metadata fields only.
fn canonical_form(value: &serde_json::Value) -> serde_json::Value {
    let mut v = value.clone();
    if let Some(obj) = v.as_object_mut() {
        obj.remove("status");
        if let Some(meta) = obj.get_mut("metadata").and_then(|m| m.as_object_mut()) {
            meta.remove("creationTimestamp");
            meta.remove("annotations");
            // Helm chart adds `app.kubernetes.io/name: kars` to the
            // ObjectMeta labels; the Rust derive emits no labels. Drop
            // labels from the comparison — they are operator-side
            // concerns and are not validated by the API server.
            meta.remove("labels");
            meta.remove("managedFields");
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One-shot dumper. Run via:
    ///
    ///   DUMP_MCP_CRD_YAML=1 cargo test --bin kars-controller \
    ///       helm_drift::tests::dump_mcp_crd_yaml -- --nocapture
    #[test]
    fn dump_mcp_crd_yaml() {
        if std::env::var("DUMP_MCP_CRD_YAML").is_err() {
            return;
        }
        let crd = mcp_server_crd();
        let yaml = serde_yaml::to_string(&crd).expect("serialize crd to YAML");
        println!("---\n{yaml}");
    }

    /// One-shot dumper for the toolpolicy CRD. Run via:
    ///
    ///   DUMP_TOOLPOLICY_CRD_YAML=1 cargo test --bin kars-controller \
    ///       helm_drift::tests::dump_toolpolicy_crd_yaml -- --nocapture
    #[test]
    fn dump_toolpolicy_crd_yaml() {
        if std::env::var("DUMP_TOOLPOLICY_CRD_YAML").is_err() {
            return;
        }
        let crd = tool_policy_crd();
        let yaml = serde_yaml::to_string(&crd).expect("serialize crd to YAML");
        println!("---\n{yaml}");
    }

    /// One-shot dumper for the a2aagent CRD. Run via:
    ///
    ///   DUMP_A2AAGENT_CRD_YAML=1 cargo test --bin kars-controller \
    ///       helm_drift::tests::dump_a2aagent_crd_yaml -- --nocapture
    #[test]
    fn dump_a2aagent_crd_yaml() {
        if std::env::var("DUMP_A2AAGENT_CRD_YAML").is_err() {
            return;
        }
        let crd = a2a_agent_crd();
        let yaml = serde_yaml::to_string(&crd).expect("serialize crd to YAML");
        println!("---\n{yaml}");
    }

    fn assert_helm_matches_rust(helm_path: &str, rust_value: serde_json::Value, label: &str) {
        let helm_text = match std::fs::read_to_string(helm_path) {
            Ok(s) => s,
            Err(_) => {
                eprintln!(
                    "helm CRD not present at {helm_path} — skipping drift check (bootstrap mode for {label})"
                );
                return;
            }
        };
        let helm_crd: serde_json::Value =
            serde_yaml::from_str(&helm_text).expect("helm crd YAML must parse as JSON value");

        let helm_canonical = canonical_form(&helm_crd);
        let rust_canonical = canonical_form(&rust_value);

        if helm_canonical != rust_canonical {
            let helm_pretty = serde_json::to_string_pretty(&helm_canonical).unwrap_or_default();
            let rust_pretty = serde_json::to_string_pretty(&rust_canonical).unwrap_or_default();
            panic!(
                "helm CRD {label} has drifted from Rust schema.\n\nHELM (canonical):\n{helm_pretty}\n\nRUST (canonical):\n{rust_pretty}"
            );
        }
    }

    #[test]
    fn helm_crd_matches_rust_schema() {
        let rust_crd_value =
            serde_json::to_value(mcp_server_crd()).expect("rust crd serializes to JSON");
        assert_helm_matches_rust(MCP_HELM_CRD_PATH, rust_crd_value, "mcpserver");
    }

    #[test]
    fn helm_toolpolicy_crd_matches_rust_schema() {
        let rust_crd_value =
            serde_json::to_value(tool_policy_crd()).expect("rust crd serializes to JSON");
        assert_helm_matches_rust(TOOLPOLICY_HELM_CRD_PATH, rust_crd_value, "toolpolicy");
    }

    #[test]
    fn helm_a2aagent_crd_matches_rust_schema() {
        let rust_crd_value =
            serde_json::to_value(a2a_agent_crd()).expect("rust crd serializes to JSON");
        assert_helm_matches_rust(A2AAGENT_HELM_CRD_PATH, rust_crd_value, "a2aagent");
    }

    /// One-shot dumper for the inferencepolicy CRD. Run via:
    ///
    ///   DUMP_INFERENCEPOLICY_CRD_YAML=1 cargo test --bin kars-controller \
    ///       helm_drift::tests::dump_inferencepolicy_crd_yaml -- --nocapture
    #[test]
    fn dump_inferencepolicy_crd_yaml() {
        if std::env::var("DUMP_INFERENCEPOLICY_CRD_YAML").is_err() {
            return;
        }
        let crd = inference_policy_crd();
        let yaml = serde_yaml::to_string(&crd).expect("serialize crd to YAML");
        println!("---\n{yaml}");
    }

    #[test]
    fn helm_inferencepolicy_crd_matches_rust_schema() {
        let rust_crd_value =
            serde_json::to_value(inference_policy_crd()).expect("rust crd serializes to JSON");
        assert_helm_matches_rust(
            INFERENCEPOLICY_HELM_CRD_PATH,
            rust_crd_value,
            "inferencepolicy",
        );
    }

    /// One-shot dumper for the karsmemory CRD. Run via:
    ///
    ///   DUMP_CLAWMEMORY_CRD_YAML=1 cargo test --bin kars-controller \
    ///       helm_drift::tests::dump_karsmemory_crd_yaml -- --nocapture
    #[test]
    fn dump_karsmemory_crd_yaml() {
        if std::env::var("DUMP_CLAWMEMORY_CRD_YAML").is_err() {
            return;
        }
        let crd = kars_memory_crd();
        let yaml = serde_yaml::to_string(&crd).expect("serialize crd to YAML");
        println!("---\n{yaml}");
    }

    #[test]
    fn helm_karsmemory_crd_matches_rust_schema() {
        let rust_crd_value =
            serde_json::to_value(kars_memory_crd()).expect("rust crd serializes to JSON");
        assert_helm_matches_rust(CLAWMEMORY_HELM_CRD_PATH, rust_crd_value, "karsmemory");
    }

    /// One-shot dumper for the karseval CRD. Run via:
    ///
    ///   DUMP_CLAWEVAL_CRD_YAML=1 cargo test --bin kars-controller \
    ///       helm_drift::tests::dump_karseval_crd_yaml -- --nocapture
    #[test]
    fn dump_karseval_crd_yaml() {
        if std::env::var("DUMP_CLAWEVAL_CRD_YAML").is_err() {
            return;
        }
        let crd = kars_eval_crd();
        let yaml = serde_yaml::to_string(&crd).expect("serialize crd to YAML");
        println!("---\n{yaml}");
    }

    #[test]
    fn helm_karseval_crd_matches_rust_schema() {
        let rust_crd_value =
            serde_json::to_value(kars_eval_crd()).expect("rust crd serializes to JSON");
        assert_helm_matches_rust(CLAWEVAL_HELM_CRD_PATH, rust_crd_value, "karseval");
    }

    /// One-shot dumper for the trustgraph CRD. Run via:
    ///
    ///   DUMP_TRUSTGRAPH_CRD_YAML=1 cargo test --bin kars-controller \
    ///       helm_drift::tests::dump_trustgraph_crd_yaml -- --nocapture
    #[test]
    fn dump_trustgraph_crd_yaml() {
        if std::env::var("DUMP_TRUSTGRAPH_CRD_YAML").is_err() {
            return;
        }
        let crd = trust_graph_crd();
        let yaml = serde_yaml::to_string(&crd).expect("serialize crd to YAML");
        println!("---\n{yaml}");
    }

    #[test]
    fn helm_trustgraph_crd_matches_rust_schema() {
        let rust_crd_value =
            serde_json::to_value(trust_graph_crd()).expect("rust crd serializes to JSON");
        assert_helm_matches_rust(TRUSTGRAPH_HELM_CRD_PATH, rust_crd_value, "trustgraph");
    }

    /// One-shot dumper for the egressapproval CRD. Run via:
    ///
    ///   DUMP_EGRESSAPPROVAL_CRD_YAML=1 cargo test --bin kars-controller \
    ///       helm_drift::tests::dump_egressapproval_crd_yaml -- --nocapture
    #[test]
    fn dump_egressapproval_crd_yaml() {
        if std::env::var("DUMP_EGRESSAPPROVAL_CRD_YAML").is_err() {
            return;
        }
        let crd = egress_approval_crd();
        let yaml = serde_yaml::to_string(&crd).expect("serialize crd to YAML");
        println!("---\n{yaml}");
    }

    #[test]
    fn helm_egressapproval_crd_matches_rust_schema() {
        let rust_crd_value =
            serde_json::to_value(egress_approval_crd()).expect("rust crd serializes to JSON");
        assert_helm_matches_rust(
            EGRESSAPPROVAL_HELM_CRD_PATH,
            rust_crd_value,
            "egressapproval",
        );
    }
}
