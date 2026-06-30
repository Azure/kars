// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Sub-agent MCP inheritance tests. Pulled out of `mod tests` inside
//! `spawn::mod` to keep that file under the §4.2 LOC budget.
//!
//! Pins `parent_mcp_server_refs` (the parent-CR extractor that honors the
//! deprecated singular `mcpServerRef` shim) and the production overlay that
//! copies the parent's `governance.mcpServerRefs` onto a spawned child so
//! sub-agents inherit MCP tool access (e.g. Playwright).

use super::*;
use std::collections::BTreeMap;

fn req(agent_id: &str) -> SpawnRequest {
    SpawnRequest {
        agent_id: agent_id.into(),
        model: None,
        governance: true,
        trust_threshold: None,
        learn_egress: false,
        isolation: None,
        token_budget_daily: None,
        token_budget_per_request: None,
        trusted_peers: None,
        handoff: None,
        runtime_kind: None,
        role: None,
    }
}

#[test]
fn parent_mcp_refs_reads_plural() {
    let data = serde_json::json!({
        "spec": { "governance": { "mcpServerRefs": [
            { "name": "playwright" },
            { "name": "filesystem" },
        ] } }
    });
    let refs = parent_mcp_server_refs(&data);
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0]["name"], "playwright");
    assert_eq!(refs[1]["name"], "filesystem");
}

#[test]
fn parent_mcp_refs_lifts_deprecated_singular() {
    let data = serde_json::json!({
        "spec": { "governance": { "mcpServerRef": { "name": "playwright" } } }
    });
    let refs = parent_mcp_server_refs(&data);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0]["name"], "playwright");
}

#[test]
fn parent_mcp_refs_prefers_plural_over_singular() {
    // Plural is authoritative when both are present (mirrors
    // GovernanceConfig::effective_mcp_server_refs).
    let data = serde_json::json!({
        "spec": { "governance": {
            "mcpServerRefs": [ { "name": "plural" } ],
            "mcpServerRef": { "name": "singular" },
        } }
    });
    let refs = parent_mcp_server_refs(&data);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0]["name"], "plural");
}

#[test]
fn parent_mcp_refs_empty_when_absent() {
    assert!(parent_mcp_server_refs(&serde_json::json!({})).is_empty());
    assert!(
        parent_mcp_server_refs(&serde_json::json!({ "spec": { "governance": {} } })).is_empty()
    );
    // Empty plural array must not mask into a stray entry.
    assert!(
        parent_mcp_server_refs(
            &serde_json::json!({ "spec": { "governance": { "mcpServerRefs": [] } } })
        )
        .is_empty()
    );
}

#[test]
fn inherited_mcp_refs_overlay_onto_child_governance() {
    // Reproduces the production overlay: build a child CRD (no MCP refs),
    // then apply the parent's inherited refs the way `create_sandbox` does.
    let mut crd = build_sub_agent_crd_with_labels(
        "parent",
        "kars-parent",
        "enhanced",
        "gpt-5.4",
        &req("child"),
        &BTreeMap::new(),
    );
    assert!(
        crd["spec"]["governance"].get("mcpServerRefs").is_none(),
        "builder must not invent MCP refs on its own"
    );

    let parent_data = serde_json::json!({
        "spec": { "governance": { "mcpServerRefs": [ { "name": "playwright" } ] } }
    });
    let refs = parent_mcp_server_refs(&parent_data);
    if !refs.is_empty()
        && let Some(gov) = crd
            .get_mut("spec")
            .and_then(|s| s.get_mut("governance"))
            .filter(|g| g.is_object())
    {
        gov["mcpServerRefs"] = serde_json::Value::Array(refs);
    }

    let got = crd["spec"]["governance"]["mcpServerRefs"]
        .as_array()
        .expect("child must carry inherited mcpServerRefs");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0]["name"], "playwright");
}
