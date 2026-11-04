// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::kars::task::{TaskBlueprint, TaskEgress};

/// Explicit member selections, bounded by the reviewed Team resources.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleResources {
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    #[serde(default)]
    pub egress: Vec<TaskEgress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
}

impl RoleResources {
    pub(super) fn from_blueprint(blueprint: &TaskBlueprint) -> Self {
        Self {
            mcp_servers: blueprint.mcp_servers.clone(),
            egress: blueprint.egress.clone(),
            memory: blueprint.memory.clone(),
        }
    }

    pub(super) fn bind(
        &self,
        role: &str,
        parent: Option<&TaskBlueprint>,
        blueprint: &mut serde_json::Map<String, serde_json::Value>,
    ) -> AppResult<()> {
        let denied = || {
            AppError::BadRequest(format!(
                "role {role} resources must be an explicit subset of the Team MCP servers and egress destinations"
            ))
        };
        let servers = super::validation::normalize_mcp_servers(&self.mcp_servers)?;
        if self.egress.len() > 16
            || servers.iter().any(|server| {
                !parent.is_some_and(|parent| {
                    parent
                        .mcp_servers
                        .iter()
                        .any(|allowed| allowed.trim() == server)
                })
            })
        {
            return Err(denied());
        }
        let mut egress = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for entry in &self.egress {
            let host = entry.host.trim().to_ascii_lowercase();
            let port = entry.port.unwrap_or(443);
            if host.is_empty()
                || !(1..=65535).contains(&port)
                || !parent.is_some_and(|parent| {
                    parent.egress.iter().any(|allowed| {
                        allowed.host.trim().eq_ignore_ascii_case(&host)
                            && allowed.port.unwrap_or(443) == port
                    })
                })
            {
                return Err(denied());
            }
            if seen.insert((host.clone(), port)) {
                egress.push(serde_json::json!({"host": host, "port": port}));
            }
        }
        if let Some(memory) = &self.memory {
            let memory = memory.trim();
            if memory.is_empty()
                || !parent.is_some_and(|parent| {
                    parent
                        .memory
                        .as_deref()
                        .is_some_and(|allowed| allowed.trim() == memory)
                })
            {
                return Err(AppError::BadRequest(format!(
                    "role {role} memory must match the explicitly selected Team memory resource"
                )));
            }
            blueprint.insert("memory".into(), serde_json::json!(memory));
        }
        blueprint.insert("mcpServers".into(), serde_json::json!(servers));
        blueprint.insert("egress".into(), serde_json::json!(egress));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::teams::{CreateRole, validation::build_roster};
    use serde_json::json;

    fn parent() -> TaskBlueprint {
        serde_json::from_value(json!({
            "runtime":"OpenClaw", "model":{"provider":"github-copilot","deployment":"principal"},
            "modelFallbacks":[{"provider":"foundry","deployment":"backup"}],
            "mcpServers":["docs","other"],
            "egress":[{"host":"docs.example","port":443},{"host":"other.example","port":8443}],
            "memory":"private-memory", "toolPolicy":"reviewed", "skills":["private-skill"]
        }))
        .unwrap()
    }

    #[test]
    fn explicit_member_resources_bind_both_runtimes_without_widening() {
        let parent = parent();
        for runtime in ["OpenClaw", "Hermes"] {
            for model in [None, Some("foundry::worker")] {
                let role: CreateRole = serde_json::from_value(json!({
                    "name":"reader", "runtime":runtime, "model":model,
                    "resources":{"mcp_servers":["docs"], "egress":[{"host":"DOCS.EXAMPLE"}]}
                }))
                .unwrap();
                let roster = build_roster(&[role], Some("reviewed"), Some(&parent)).unwrap();
                let bp = &roster[0]["blueprint"];
                assert_eq!(bp["runtime"], runtime);
                assert_eq!(bp["mcpServers"], json!(["docs"]));
                assert_eq!(bp["egress"], json!([{"host":"docs.example","port":443}]));
                assert_eq!(
                    bp["model"]["deployment"],
                    if model.is_some() {
                        "worker"
                    } else {
                        "principal"
                    }
                );
                assert_eq!(bp["modelFallbacks"].is_array(), model.is_none());
                assert_eq!(bp["toolPolicy"], "reviewed");
                for field in [
                    "memory",
                    "skills",
                    "gitWrite",
                    "executionPlan",
                    "egressMode",
                ] {
                    assert!(bp.get(field).is_none(), "must not copy {field}");
                }
            }
        }
        assert_eq!(parent.mcp_servers, ["docs", "other"]);
    }

    #[test]
    fn explicit_empty_member_resources_do_not_inherit_without_route_overrides() {
        let role: CreateRole =
            serde_json::from_value(json!({"name":"reviewer","resources":{}})).unwrap();
        let roster = build_roster(&[role], Some("reviewed"), Some(&parent())).unwrap();
        assert_eq!(roster[0]["blueprint"]["runtime"], "OpenClaw");
        assert_eq!(roster[0]["blueprint"]["mcpServers"], json!([]));
        assert_eq!(roster[0]["blueprint"]["egress"], json!([]));
        let legacy: CreateRole = serde_json::from_value(json!({"name":"reviewer"})).unwrap();
        assert!(
            build_roster(&[legacy], Some("reviewed"), Some(&parent())).unwrap()[0]
                .get("blueprint")
                .is_none()
        );
    }

    #[test]
    fn member_resources_reject_unselected_server_host_port_and_unknown_fields() {
        for resources in [
            json!({"mcp_servers":["unselected"]}),
            json!({"egress":[{"host":"unselected.example","port":443}]}),
            json!({"egress":[{"host":"docs.example","port":8443}]}),
            json!({"egress":[{"host":"docs.example","port":0}]}),
            json!({"egress":[{"host":"docs.example","port":65536}]}),
        ] {
            let role: CreateRole =
                serde_json::from_value(json!({"name":"reader","resources":resources})).unwrap();
            assert!(build_roster(&[role], None, Some(&parent())).is_err());
        }
        assert!(serde_json::from_value::<RoleResources>(json!({"unknown":"unreviewed"})).is_err());
    }

    #[test]
    fn member_memory_is_explicit_bounded_and_retained_for_both_runtimes() {
        let parent = parent();
        for runtime in ["OpenClaw", "Hermes"] {
            let role: CreateRole = serde_json::from_value(json!({
                "name":"reader", "runtime":runtime,
                "resources":{"memory":" private-memory "}
            }))
            .unwrap();
            let roster = build_roster(&[role], Some("reviewed"), Some(&parent)).unwrap();
            let stored: TaskBlueprint =
                serde_json::from_value(roster[0]["blueprint"].clone()).unwrap();
            assert_eq!(stored.runtime.as_deref(), Some(runtime));
            assert_eq!(stored.memory.as_deref(), Some("private-memory"));
            assert!(stored.mcp_servers.is_empty());
            assert!(stored.egress.is_empty());
            let readback = RoleResources::from_blueprint(&stored);
            assert_eq!(readback.memory.as_deref(), Some("private-memory"));
            let mut roundtrip = serde_json::Map::new();
            readback
                .bind("reader", Some(&parent), &mut roundtrip)
                .unwrap();
            assert_eq!(roundtrip["memory"], "private-memory");
            let mut removed = parent.clone();
            removed.memory = None;
            assert!(
                readback
                    .bind("reader", Some(&removed), &mut roundtrip)
                    .is_err()
            );
            removed.memory = Some("replacement".into());
            assert!(
                readback
                    .bind("reader", Some(&removed), &mut roundtrip)
                    .is_err()
            );
            assert!(readback.bind("reader", None, &mut roundtrip).is_err());
        }
        for value in [json!("unreviewed"), json!(""), json!(" ")] {
            let role: CreateRole = serde_json::from_value(json!({
                "name":"reader", "resources":{"memory":value}
            }))
            .unwrap();
            assert!(build_roster(&[role], None, Some(&parent)).is_err());
        }
        assert!(serde_json::from_value::<RoleResources>(json!({"memory":true})).is_err());
        let none: RoleResources = serde_json::from_value(json!({"memory":null})).unwrap();
        let mut explicit = serde_json::Map::new();
        none.bind("reader", Some(&parent), &mut explicit).unwrap();
        assert!(!explicit.contains_key("memory"));
    }

    #[test]
    fn member_resource_limits_and_types_fail_closed() {
        let mut parent = parent();
        parent.mcp_servers = (0..9).map(|n| format!("server-{n}")).collect();
        let selected = RoleResources {
            mcp_servers: parent.mcp_servers.clone(),
            egress: vec![],
            memory: None,
        };
        assert!(
            selected
                .bind("reader", Some(&parent), &mut serde_json::Map::new())
                .is_err()
        );
        let selected = RoleResources {
            mcp_servers: vec![],
            egress: vec![parent.egress[0].clone(); 17],
            memory: None,
        };
        assert!(
            selected
                .bind("reader", Some(&parent), &mut serde_json::Map::new())
                .is_err()
        );
        for raw in [
            json!(null),
            json!({"mcp_servers":"docs"}),
            json!({"egress":[{"host":"docs.example","port":"443"}]}),
        ] {
            assert!(serde_json::from_value::<RoleResources>(raw).is_err());
        }
    }

    #[test]
    fn member_resource_readback_roundtrip_and_parent_removal_are_checked() {
        let parent = parent();
        let mut bp = serde_json::Map::new();
        let selected: RoleResources = serde_json::from_value(json!({
            "mcp_servers":[" docs ","docs"],
            "egress":[{"host":" DOCS.EXAMPLE ","port":443},{"host":"docs.example"}]
        }))
        .unwrap();
        selected.bind("reader", Some(&parent), &mut bp).unwrap();
        let stored: TaskBlueprint = serde_json::from_value(bp.clone().into()).unwrap();
        let readback = RoleResources::from_blueprint(&stored);
        let mut roundtrip = serde_json::Map::new();
        readback
            .bind("reader", Some(&parent), &mut roundtrip)
            .unwrap();
        assert_eq!(bp, roundtrip);
        let mut reduced = parent.clone();
        reduced.mcp_servers.clear();
        assert!(
            readback
                .bind("reader", Some(&reduced), &mut roundtrip)
                .is_err()
        );
        reduced = parent.clone();
        reduced.egress.clear();
        assert!(
            readback
                .bind("reader", Some(&reduced), &mut roundtrip)
                .is_err()
        );
        assert!(readback.bind("reader", None, &mut roundtrip).is_err());
    }
}
