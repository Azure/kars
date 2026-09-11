use super::client::extract_json;
use super::egress::complete_egress_recommendation;
use super::execution::parse_execution_plan;
use super::routing::{
    catalogue_has_model_key, efficiency_basis, efficient_member_route_is_qualified,
    recommendation_is_actionable, select_orchestrator_route, should_strengthen_team_principal,
};
use super::team_qualification::default_model_route;
use super::{
    ComposeEgress, ComposeTeamMilestone, ComposeTeamProposal, ComposeTeamRole,
    is_non_autonomous_harness,
};

pub(super) fn is_synthesis_only_team_role(name: &str, system_prompt: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let prompt = system_prompt.to_ascii_lowercase();
    let explicitly_reconciles_handbacks = [
        "reconcile specialist handbacks",
        "reconcile the specialist handbacks",
        "synthesize specialist handbacks",
        "synthesize the specialist handbacks",
        "combine specialist handbacks",
        "combine the specialist handbacks",
    ]
    .iter()
    .any(|phrase| prompt.contains(phrase));
    let principal_like_name = [
        "readiness-editor",
        "synthesis-editor",
        "final-synthesizer",
        "report-integrator",
    ]
    .contains(&name.as_str());

    explicitly_reconciles_handbacks
        || (principal_like_name
            && ["final report", "final synthesis", "principal deliverable"]
                .iter()
                .any(|phrase| prompt.contains(phrase)))
}

/// Validate the team orchestrator's JSON against real options — runtimes and
/// models must exist (or be empty for the default); tier/cadence clamped.
pub(super) fn parse_and_validate_team(
    raw: &str,
    o: &crate::routes::options::Options,
    eff: &crate::routes::efficiency::EfficiencyDto,
    charter: &str,
) -> (ComposeTeamProposal, Option<String>) {
    let json = extract_json(raw).unwrap_or_else(|| serde_json::json!({}));

    let tier = json
        .get("tier")
        .and_then(|v| v.as_i64())
        .map(|t| t.clamp(1, 5) as i32)
        .unwrap_or(3);
    let cadence_minutes = json
        .get("cadence_minutes")
        .and_then(|v| v.as_i64())
        .filter(|c| *c >= 0)
        .unwrap_or(0);
    let mut instructions = json
        .get("instructions")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let mcp_servers = json
        .get("mcp_servers")
        .and_then(|v| v.as_array())
        .map(|servers| {
            servers
                .iter()
                .filter_map(|server| server.as_str().map(str::trim))
                .filter(|server| o.mcp_servers.iter().any(|option| option.name == *server))
                .scan(std::collections::BTreeSet::new(), |seen, server| {
                    seen.insert(server.to_string()).then(|| server.to_string())
                })
                .take(8)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let requested_memory = json
        .get("memory")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|memory| !memory.is_empty())
        .filter(|memory| o.memories.iter().any(|option| option.name == *memory))
        .map(str::to_string);

    let valid_runtime = |rt: &str| o.runtimes.iter().any(|r| r.wired && r.kind == rt);
    let valid_model = |model: &str| catalogue_has_model_key(&o.models, model);
    let mut model = json
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|model| valid_model(model))
        .unwrap_or("")
        .to_string();
    let selected_deployment = model
        .split_once("::")
        .map(|(_, deployment)| deployment)
        .unwrap_or("");
    let selected_efficiency = eff
        .routes
        .iter()
        .find(|route| route.route == selected_deployment);
    let mut model_basis = if selected_deployment.is_empty() {
        Some("Team default — no explicit principal model was proposed.".to_string())
    } else if recommendation_is_actionable(
        eff.recommended.as_deref(),
        eff.recommended_low_confidence,
    ) && eff
        .recommended
        .as_deref()
        .is_some_and(|recommended| recommended == selected_deployment)
    {
        Some(efficiency_basis(eff, selected_deployment))
    } else if selected_efficiency.is_some() {
        Some(
            "Chosen by the org orchestrator for this charter; historical route evidence is shown for comparison."
                .to_string(),
        )
    } else {
        Some("Chosen by the org orchestrator for this charter; no retained route history is available yet.".to_string())
    };
    let principal_runtime = "OpenClaw";
    let principal_route = model
        .split_once("::")
        .map(|(provider, deployment)| (provider.to_string(), deployment.to_string()))
        .or_else(|| {
            default_model_route(o).and_then(|route| {
                route
                    .split_once("::")
                    .map(|(provider, deployment)| (provider.to_string(), deployment.to_string()))
            })
        });
    let memory = if let Some(memory) = requested_memory {
        Some(memory)
    } else if let Some((provider, deployment)) = principal_route.as_ref() {
        o.memories.iter().find_map(|option| {
            let foundry_like = option
                .backend
                .as_deref()
                .is_some_and(|backend| backend.to_ascii_lowercase().contains("foundry"));
            let ready = option.readiness.as_deref().is_some_and(|readiness| {
                readiness == "Ready" || readiness.starts_with("Ready=True")
            });
            let qualified = crate::routes::options::memory_binding_qualified_for_route(
                principal_runtime,
                provider,
                deployment,
                option,
            )
            .unwrap_or(false);
            (foundry_like && ready && qualified).then(|| option.name.clone())
        })
    } else {
        None
    };
    let egress = json
        .get("egress")
        .and_then(|value| value.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let host = entry.get("host")?.as_str()?.trim();
                    let valid = !host.is_empty()
                        && host.contains('.')
                        && host
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'));
                    valid.then(|| ComposeEgress {
                        host: host.to_ascii_lowercase(),
                        port: entry
                            .get("port")
                            .and_then(|port| port.as_u64())
                            .and_then(|port| u16::try_from(port).ok()),
                    })
                })
                .take(16)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let egress = complete_egress_recommendation(egress, charter, &mcp_servers);
    let egress_mode = match json
        .get("egress_mode")
        .and_then(|value| value.as_str())
        .unwrap_or("learning")
        .to_ascii_lowercase()
        .as_str()
    {
        "strict" => "strict",
        _ => "learning",
    }
    .to_string();

    let mut roles = json
        .get("roles")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    let name = r.get("name").and_then(|v| v.as_str())?.trim().to_string();
                    if name.is_empty() || name.eq_ignore_ascii_case("principal") {
                        return None;
                    }
                    let system_prompt = r
                        .get("system_prompt")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if is_synthesis_only_team_role(&name, &system_prompt) {
                        return None;
                    }
                    // Harness capability: a bootstrap-only adapter can't run a
                    // standing member autonomously — correct it to OpenClaw so the
                    // role actually produces work (Hermes/BYO pass through).
                    let runtime = {
                        let rt = r
                            .get("runtime")
                            .and_then(|v| v.as_str())
                            .map(str::trim)
                            .filter(|s| valid_runtime(s))
                            .unwrap_or("")
                            .to_string();
                        if is_non_autonomous_harness(&rt) {
                            "OpenClaw".to_string()
                        } else {
                            rt
                        }
                    };
                    let model = r
                        .get("model")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| valid_model(s))
                        .unwrap_or("")
                        .to_string();
                    let skills = r
                        .get("skills")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|s| s.as_str())
                                .filter(|skill| o.skills.iter().any(|option| option.name == *skill))
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default();
                    Some(ComposeTeamRole {
                        name,
                        system_prompt,
                        runtime,
                        model,
                        skills,
                    })
                })
                .take(6)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let execution_plan = parse_execution_plan(&json).inspect(|plan| {
        let proposed_roles = roles.clone();
        roles = plan
            .roles
            .iter()
            .enumerate()
            .map(|(index, planned_role)| {
                let mut role = proposed_roles
                    .iter()
                    .find(|role| role.name == planned_role.name)
                    .cloned()
                    .or_else(|| proposed_roles.get(index).cloned())
                    .unwrap_or_else(|| ComposeTeamRole {
                        name: planned_role.name.clone(),
                        system_prompt: planned_role.objective.clone(),
                        runtime: String::new(),
                        model: String::new(),
                        skills: Vec::new(),
                    });
                role.name = planned_role.name.clone();
                if role.system_prompt.trim().is_empty() {
                    role.system_prompt = planned_role.objective.clone();
                }
                role
            })
            .collect();
    });
    let role_names = roles
        .iter()
        .map(|role| role.name.as_str())
        .collect::<std::collections::HashSet<_>>();
    let mut seen_milestones = std::collections::BTreeSet::new();
    let normalize_milestone_id = |value: &str| {
        value
            .trim()
            .to_ascii_lowercase()
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' {
                    character
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .trim_matches('-')
            .chars()
            .take(63)
            .collect::<String>()
    };
    let mut milestones_invalid = false;
    let milestones = json
        .get("milestones")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let id = normalize_milestone_id(
                        entry.get("id").and_then(serde_json::Value::as_str)?,
                    );
                    let title = entry
                        .get("title")
                        .and_then(serde_json::Value::as_str)?
                        .trim()
                        .to_string();
                    if id.is_empty() || title.is_empty() || seen_milestones.contains(&id) {
                        return None;
                    }
                    let requested_dependencies = entry
                        .get("depends_on")
                        .and_then(serde_json::Value::as_array)
                        .map(|dependencies| {
                            dependencies
                                .iter()
                                .filter_map(serde_json::Value::as_str)
                                .map(normalize_milestone_id)
                                .filter(|dependency| !dependency.is_empty())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    if requested_dependencies
                        .iter()
                        .any(|dependency| !seen_milestones.contains(dependency))
                    {
                        milestones_invalid = true;
                        return None;
                    }
                    let depends_on = requested_dependencies;
                    let acceptance_criteria = entry
                        .get("acceptance_criteria")
                        .and_then(serde_json::Value::as_array)
                        .map(|criteria| {
                            criteria
                                .iter()
                                .filter_map(serde_json::Value::as_str)
                                .map(str::trim)
                                .filter(|criterion| !criterion.is_empty())
                                .take(20)
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let owner_role = entry
                        .get("owner_role")
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .filter(|owner| role_names.contains(*owner))
                        .map(str::to_string);
                    let description = entry
                        .get("description")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    let review_required = entry
                        .get("review_required")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    seen_milestones.insert(id.clone());
                    Some(ComposeTeamMilestone {
                        id,
                        title,
                        description,
                        owner_role,
                        depends_on,
                        acceptance_criteria,
                        review_required,
                    })
                })
                .take(8)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if milestones_invalid {
        instructions.clear();
    }

    let mut rationale = json
        .get("rationale")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some((provider, deployment, basis)) = select_orchestrator_route(o, eff) {
        let current_route = if model.is_empty() {
            o.models
                .iter()
                .find(|option| option.is_default)
                .map(|option| format!("{}::{}", option.provider, option.deployment))
                .unwrap_or_default()
        } else {
            model.clone()
        };
        let current_deployment = current_route
            .split_once("::")
            .map(|(_, deployment)| deployment)
            .unwrap_or("");
        if should_strengthen_team_principal(roles.len(), current_deployment, &deployment) {
            let current_evidence = eff
                .routes
                .iter()
                .find(|route| route.route == current_deployment);
            let keep_efficient_members = current_evidence.is_some_and(|route| {
                efficient_member_route_is_qualified(route.runs, route.acceptance_rate)
            });
            let frontier_route = format!("{provider}::{deployment}");
            let member_route = if keep_efficient_members {
                current_route.clone()
            } else {
                frontier_route.clone()
            };
            for role in &mut roles {
                if role.model.is_empty() {
                    role.model = member_route.clone();
                }
            }
            model = frontier_route;
            let member_basis = if keep_efficient_members {
                format!(
                    "member roles retain the qualified {} route ({} historical run(s))",
                    current_deployment,
                    current_evidence.map(|route| route.runs).unwrap_or(0)
                )
            } else {
                format!(
                    "member roles also use {} until the proposed {} route has enough accepted outcomes to qualify",
                    deployment, current_deployment
                )
            };
            model_basis = Some(format!(
                "{basis} The principal coordinates {} independent roles; {member_basis}.",
                roles.len(),
            ));
            let note = format!(
                "The principal route was strengthened to {deployment} for multi-role orchestration reliability; {member_basis}."
            );
            rationale = Some(match rationale {
                Some(existing) => format!("{existing} {note}"),
                None => note,
            });
        }
    }
    let engineering_enabled = json
        .get("engineering_enabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let allowed_engineering_signals = [
        "dependabot_pr",
        "dependabot_alert",
        "code_scanning_alert",
        "secret_scanning_alert",
    ];
    let engineering_signals = json
        .get("engineering_signals")
        .and_then(serde_json::Value::as_array)
        .map(|signals| {
            signals
                .iter()
                .filter_map(serde_json::Value::as_str)
                .filter(|signal| allowed_engineering_signals.contains(signal))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let engineering_poll_interval_seconds = json
        .get("engineering_poll_interval_seconds")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(900)
        .clamp(300, 86_400);
    let engineering_auto_run = json
        .get("engineering_auto_run")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    (
        ComposeTeamProposal {
            tier,
            cadence_minutes,
            instructions,
            model,
            model_fallbacks: Vec::new(),
            model_basis,
            expected_tokens_per_outcome: selected_efficiency
                .map(|route| route.tokens_per_outcome)
                .filter(|tokens| *tokens > 0),
            efficiency_sample_runs: selected_efficiency.map(|route| route.runs).unwrap_or(0),
            mcp_servers,
            memory,
            egress,
            egress_mode,
            engineering_enabled: engineering_enabled && !engineering_signals.is_empty(),
            engineering_signals,
            engineering_poll_interval_seconds,
            engineering_auto_run,
            roles,
            execution_plan,
            milestones,
        },
        rationale,
    )
}
