// kars Bridge BFF — resources pre-flight checks.

use super::network::{resolves, url_host};
use super::{Check, CheckStatus};

fn names(items: &[kube::core::DynamicObject]) -> Vec<String> {
    items
        .iter()
        .filter_map(|o| o.metadata.name.clone())
        .collect()
}

/// Readiness facts read from a CRD object's status: the `phase`, whether a
/// `Ready` condition is `True`, and that condition's message (the controller's
/// own honest reason). This is the §19 "provisioned ≠ usable" signal — the
/// controller reconciles + (for MCP) probes these, so we report its real
/// verdict rather than a fabricated one.
struct Readiness {
    phase: Option<String>,
    ready: Option<bool>,
    message: Option<String>,
}

fn readiness_of(items: &[kube::core::DynamicObject], name: &str) -> Option<Readiness> {
    let obj = items
        .iter()
        .find(|o| o.metadata.name.as_deref() == Some(name))?;
    let status = obj.data.get("status");
    let phase = status
        .and_then(|s| s.get("phase"))
        .and_then(|p| p.as_str())
        .map(|s| s.to_string());
    let ready_cond = status
        .and_then(|s| s.get("conditions"))
        .and_then(|c| c.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|c| c.get("type").and_then(|t| t.as_str()) == Some("Ready"))
        });
    let ready = ready_cond
        .and_then(|c| c.get("status"))
        .and_then(|s| s.as_str())
        .map(|s| s == "True");
    let message = ready_cond
        .and_then(|c| c.get("message"))
        .and_then(|m| m.as_str())
        .map(|s| s.to_string());
    Some(Readiness {
        phase,
        ready,
        message,
    })
}

fn status_observes_current_generation(resource: &kube::core::DynamicObject) -> bool {
    resource
        .data
        .get("status")
        .and_then(|status| status.get("observedGeneration"))
        .and_then(|generation| generation.as_i64())
        == resource.metadata.generation
}

pub(super) async fn check_resources(
    cluster: &crate::kars::cluster::Cluster,
    namespace: &str,
    bp: &crate::routes::tasks::BlueprintDto,
    checks: &mut Vec<Check>,
) {
    // 1. Tool policy — provisioned AND usable (compiled). A ToolPolicy that
    //    exists but hasn't compiled its AGT profile can't actually govern tools.
    if let Some(tp) = bp.tool_policy.as_deref().filter(|s| !s.is_empty()) {
        let found = cluster
            .get_kind(namespace, "ToolPolicy", tp)
            .await
            .ok()
            .flatten();
        checks.push(match found.as_ref() {
            None => Check {
                id: "tool_policy".into(),
                label: format!("Tool policy “{tp}” not found"),
                status: CheckStatus::Fail,
                detail: "No ToolPolicy by that name exists. Pick an existing policy or create it in the Operator Console.".into(),
            },
            Some(resource) => {
                let r = readiness_of(std::slice::from_ref(resource), tp)
                    .expect("resource was supplied");
                // The policy compiles its AGT profile to a `Compiled` phase; a
                // `Ready=True` condition only appears once a sandbox references
                // it, so `Compiled` (or Ready) is the usable signal here.
                let compiled = r
                    .phase
                    .as_deref()
                    .map(|p| p == "Compiled" || p == "Ready")
                    .unwrap_or(false);
                if compiled && status_observes_current_generation(resource) {
                    Check {
                        id: "tool_policy".into(),
                        label: format!("Tool policy “{tp}” is compiled and usable"),
                        status: CheckStatus::Pass,
                        detail: "The ToolPolicy exists and its governance profile compiled — it can bound tool calls.".into(),
                    }
                } else {
                    Check {
                        id: "tool_policy".into(),
                        label: format!("Tool policy “{tp}” isn't compiled yet"),
                        status: CheckStatus::Warn,
                        detail: format!(
                            "The ToolPolicy exists but its current generation is not compiled (phase {}).",
                            r.phase.as_deref().unwrap_or("unknown")
                        ),
                    }
                }
            }
        });
    } else {
        // No tool policy pinned in the blueprint. The controller applies a
        // cluster-default ToolPolicy at launch, but this package doesn't specify
        // one — surface it as a Warn rather than silently passing, so "launch-
        // ready" never hides an unpinned governance boundary.
        checks.push(Check {
            id: "tool_policy".into(),
            label: "No tool policy pinned in this package".into(),
            status: CheckStatus::Warn,
            detail: "The cluster's default ToolPolicy will be applied at launch. Pin an explicit policy here if you need this package's tool-governance boundary to be reviewable and reproducible.".into(),
        });
    }

    // 2. MCP servers — managed Ready means the controller completed a real MCP
    //    initialize/tools-list probe and recorded a schema digest. External
    //    registrations remain explicitly registration-only until a sandbox call.
    if !bp.mcp_servers.is_empty() {
        for m in &bp.mcp_servers {
            let found = cluster
                .get_kind(namespace, "McpServer", m)
                .await
                .ok()
                .flatten();
            let url = found
                .as_ref()
                .and_then(|resource| {
                    resource
                        .data
                        .get("status")
                        .and_then(|status| status.get("endpoint"))
                        .or_else(|| resource.data.get("spec").and_then(|spec| spec.get("url")))
                })
                .and_then(|url| url.as_str());
            checks.push(match found.as_ref() {
                None => Check {
                    id: format!("mcp:{m}"),
                    label: format!("Connected service “{m}” not found"),
                    status: CheckStatus::Fail,
                    detail: format!(
                        "No McpServer by that name exists in namespace `{namespace}`."
                    ),
                },
                Some(resource) => {
                    let r = readiness_of(std::slice::from_ref(resource), m)
                        .expect("resource was supplied");
                    let ready = (r.ready.unwrap_or(false)
                        || r.phase.as_deref() == Some("Ready"))
                        && status_observes_current_generation(resource);
                    if ready {
                        let verified_tools = resource
                            .data
                            .get("status")
                            .and_then(|s| s.get("discoveredTools"))
                            .and_then(|v| v.as_array())
                            .map_or(0, Vec::len);
                        Check {
                            id: format!("mcp:{m}"),
                            label: format!("Connected service “{m}” is registered and reconciled"),
                            status: CheckStatus::Pass,
                            detail: if verified_tools > 0 {
                                format!(
                                    "The managed MCP workload is Ready and its live initialize/tools-list probe verified {verified_tools} tools. Mission launch still proves the sandbox-router call path."
                                )
                            } else {
                                "The external endpoint is registered and reconciled. Its credentials and real tool call are verified from the launched sandbox, not assumed here.".into()
                            },
                        }
                    } else {
                        Check {
                            id: format!("mcp:{m}"),
                            label: format!("Connected service “{m}” isn't reconciled"),
                            status: CheckStatus::Fail,
                            detail: format!(
                                "The McpServer exists but the controller hasn't reconciled it to Ready ({}). Tool calls to it would likely be denied.",
                                r.message.or(r.phase).unwrap_or_else(|| "no status".into())
                            ),
                        }
                    }
                }
            });

            // Real endpoint-reachability signal: resolve the MCP server's URL
            // host. Catches a misconfigured/typo'd endpoint honestly.
            if let Some(host) = url.and_then(url_host) {
                let resolved = resolves(&host, 443).await;
                checks.push(Check {
                    id: format!("mcp_endpoint:{m}"),
                    label: if resolved {
                        format!("“{m}” endpoint {host} resolves")
                    } else {
                        format!("“{m}” endpoint {host} does not resolve")
                    },
                    status: if resolved { CheckStatus::Pass } else { CheckStatus::Warn },
                    detail: if resolved {
                        "The MCP server's URL host resolves in DNS. A full handshake is a deeper probe.".into()
                    } else {
                        "The MCP server's URL host did not resolve from the gateway. Check the endpoint URL; it may still be reachable from inside the cluster.".into()
                    },
                });
            }
        }
        if bp
            .tool_policy
            .as_deref()
            .filter(|s| !s.is_empty())
            .is_none()
        {
            checks.push(Check {
                id: "mcp_needs_policy".into(),
                label: "Connected services need a tool policy".into(),
                status: CheckStatus::Fail,
                detail: "Governed MCP access must be bounded by a tool policy (admission enforces this).".into(),
            });
        }
    }

    // 3. Shared memory exists.
    if let Some(mem) = bp.memory.as_deref().filter(|s| !s.is_empty()) {
        let existing = cluster
            .list_kind_all("KarsMemory")
            .await
            .map(|v| names(&v))
            .unwrap_or_default();
        checks.push(if existing.iter().any(|n| n == mem) {
            Check {
                id: "memory".into(),
                label: format!("Shared memory “{mem}” exists"),
                status: CheckStatus::Pass,
                detail: "The KarsMemory store is present.".into(),
            }
        } else {
            Check {
                id: "memory".into(),
                label: format!("Shared memory “{mem}” not found"),
                status: CheckStatus::Fail,
                detail: "No KarsMemory by that name exists.".into(),
            }
        });
    }

    // 3b. (Removed) Harness-suitability check that flagged Hermes as a
    //     chat-gateway which would sit idle on a one-shot autonomous mission.
    //     Hermes now runs the agent in-process like OpenClaw and executes
    //     autonomous missions + genuine mesh delegation (verified E2E), so the
    //     warning was a stale false-positive. Both wired harnesses are valid
    //     mission runners; there is no longer a harness-suitability failure to
    //     surface here.

    // 3c. Skills — each required capability bundle (KarsSkill) must exist AND be
    //     APPROVED, because the controller's trust gate refuses to mount an
    //     unapproved skill into the sandbox. Pre-flighting this turns a silent
    //     "the agent never got the skill it needed" runtime gap into an explicit,
    //     fixable launch check (get the operator to approve it first).
    if !bp.skills.is_empty() {
        for s in &bp.skills {
            let found = cluster
                .get_kind(namespace, "KarsSkill", s)
                .await
                .ok()
                .flatten();
            let approved = found.as_ref().is_some_and(|o| {
                o.metadata
                    .annotations
                    .as_ref()
                    .and_then(|a| a.get("kars.azure.com/skill-review"))
                    .is_some_and(|review| review == "approved")
            });
            if approved {
                let required_mcp = found
                    .as_ref()
                    .and_then(|o| o.data.get("spec"))
                    .and_then(|spec| spec.get("mcpServers"))
                    .and_then(|servers| servers.as_array())
                    .cloned()
                    .unwrap_or_default();
                for server in required_mcp.iter().filter_map(|value| value.as_str()) {
                    let dependency = cluster
                        .get_kind(namespace, "McpServer", server)
                        .await
                        .ok()
                        .flatten();
                    let ready = dependency.as_ref().is_some_and(|resource| {
                        readiness_of(std::slice::from_ref(resource), server).is_some_and(|status| {
                            (status.ready.unwrap_or(false)
                                || status.phase.as_deref() == Some("Ready"))
                                && status_observes_current_generation(resource)
                        })
                    });
                    checks.push(Check {
                        id: format!("skill_mcp:{s}:{server}"),
                        label: format!("Skill “{s}” dependency “{server}”"),
                        status: if ready {
                            CheckStatus::Pass
                        } else {
                            CheckStatus::Fail
                        },
                        detail: if ready {
                            "The skill's required MCP server is Ready in this namespace.".into()
                        } else {
                            "The skill requires an MCP server that is missing, stale, or not Ready in this namespace.".into()
                        },
                    });
                }
            }
            checks.push(match found.as_ref() {
                None => Check {
                    id: format!("skill:{s}"),
                    label: format!("Skill “{s}” not found"),
                    status: CheckStatus::Fail,
                    detail: "No KarsSkill by that name exists — upload it in Skills, then have an operator approve it.".into(),
                },
                Some(_) => {
                    if approved {
                        Check {
                            id: format!("skill:{s}"),
                            label: format!("Skill “{s}” is approved and mountable"),
                            status: CheckStatus::Pass,
                            detail: "The skill package is approved; the controller will mount it into the sandbox.".into(),
                        }
                    } else {
                        Check {
                            id: format!("skill:{s}"),
                            label: format!("Skill “{s}” is not approved"),
                            status: CheckStatus::Fail,
                            detail: "The skill exists but hasn't been approved — the sandbox trust gate will refuse to mount it. An operator must approve it before the agent can use it.".into(),
                        }
                    }
                }
            });
        }
    }
}
