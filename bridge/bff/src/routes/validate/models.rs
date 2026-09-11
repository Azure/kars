// kars Bridge BFF — models pre-flight checks.

use super::{Check, CheckStatus};

pub(crate) fn qualification_requirements(
    bp: &crate::routes::tasks::BlueprintDto,
    workload: Option<&str>,
) -> (std::collections::BTreeSet<String>, i32) {
    let mut capabilities = std::collections::BTreeSet::new();
    if !bp.egress.is_empty() {
        capabilities.insert("network".into());
    }
    if !bp.mcp_servers.is_empty() {
        capabilities.insert("mcp".into());
    }
    if bp.memory.is_some() {
        capabilities.insert("memory".into());
    }
    let max_parallel = if let Some(plan) = bp.execution_plan.as_ref() {
        capabilities.insert("delegation".into());
        if !plan.deliverables.is_empty() {
            capabilities.insert("artifacts".into());
        }
        for role in &plan.roles {
            for phase in &role.phases {
                capabilities.extend(phase.capabilities.iter().cloned());
            }
        }
        capabilities.extend(plan.synthesis.capabilities.iter().cloned());
        plan.max_parallel
    } else {
        capabilities.insert("single-agent".into());
        1
    };
    if workload.is_some_and(|value| value.eq_ignore_ascii_case("team")) {
        capabilities.insert("team".into());
    }
    capabilities.insert("telemetry".into());
    (capabilities, max_parallel)
}

pub(super) async fn check_models(
    cluster: &crate::kars::cluster::Cluster,
    bp: &crate::routes::tasks::BlueprintDto,
    budget_tokens: Option<i64>,
    workload: Option<&str>,
    checks: &mut Vec<Check>,
) {
    // 4. Model is one the cluster serves. Use the exact same provider-aware
    // catalogue as the composer/picker; checking only the controller default
    // catalogue incorrectly rejects additional providers such as Foundry.
    let effective_default_model = if bp.model.is_none() {
        crate::routes::options::build_options(cluster)
            .await
            .ok()
            .and_then(|options| {
                options
                    .models
                    .iter()
                    .find(|model| model.is_default)
                    .or_else(|| options.models.first())
                    .map(|model| crate::routes::tasks::ModelDto {
                        provider: model.provider.clone(),
                        deployment: model.deployment.clone(),
                    })
            })
    } else {
        None
    };
    if let Some(model) = bp.model.as_ref().or(effective_default_model.as_ref()) {
        match crate::routes::options::build_options(cluster).await {
            Ok(options) => {
                let known = options.models.iter().any(|option| {
                    option.provider == model.provider && option.deployment == model.deployment
                });
                checks.push(Check {
                    id: "model".into(),
                    label: if known {
                        format!(
                            "Model “{}” is served through {}",
                            model.deployment, model.provider
                        )
                    } else {
                        format!(
                            "Model route “{}::{}” is not served by this cluster",
                            model.provider, model.deployment
                        )
                    },
                    status: if known {
                        CheckStatus::Pass
                    } else {
                        CheckStatus::Fail
                    },
                    detail: if known {
                        "This exact provider and deployment pair is present in the live model catalogue used by the picker.".into()
                    } else {
                        "This exact provider and deployment pair is absent from the live model catalogue. Select a listed route before launch.".into()
                    },
                });
                if known {
                    let runtime = bp.runtime.as_deref().unwrap_or("OpenClaw");
                    let (required_capabilities, max_parallel) =
                        qualification_requirements(bp, workload);
                    let qualification = crate::routes::options::route_qualification(
                        runtime,
                        &model.provider,
                        &model.deployment,
                        &required_capabilities,
                        max_parallel,
                        budget_tokens,
                    );
                    let qualified = qualification.as_ref().copied().unwrap_or(false);
                    checks.push(Check {
                        id: "route_qualification".into(),
                        label: if qualified {
                            format!(
                                "{runtime} · {}::{} is qualified",
                                model.provider, model.deployment
                            )
                        } else {
                            format!(
                                "{runtime} · {}::{} is not qualified",
                                model.provider, model.deployment
                            )
                        },
                        status: if qualified {
                            CheckStatus::Pass
                        } else {
                            CheckStatus::Fail
                        },
                        detail: if let Err(error) = qualification {
                            format!("Route qualification configuration error: {error}")
                        } else if qualified {
                            format!(
                                "This route has verified evidence for capabilities: {}.",
                                required_capabilities.iter().cloned().collect::<Vec<_>>().join(", ")
                            )
                        } else {
                            let missing = crate::routes::options::route_qualification_gap(
                                runtime,
                                &model.provider,
                                &model.deployment,
                                &required_capabilities,
                                max_parallel,
                                budget_tokens,
                            )
                            .unwrap_or_else(|_| required_capabilities.clone());
                            format!(
                                "This route lacks verified evidence for: {}. Existing retained evidence covers the other required capabilities, but qualification records are atomic and cannot be combined.",
                                missing.iter().cloned().collect::<Vec<_>>().join(", ")
                            )
                        },
                    });
                    fn find_resource<'a>(
                        items: &'a [crate::routes::options::RefOption],
                        name: &str,
                    ) -> Option<&'a crate::routes::options::RefOption> {
                        items.iter().find(|option| option.name == name)
                    }
                    for server in &bp.mcp_servers {
                        let qualification = find_resource(&options.mcp_servers, server)
                            .map(|option| {
                                crate::routes::options::mcp_server_qualified_for_route(
                                    runtime,
                                    &model.provider,
                                    &model.deployment,
                                    option,
                                )
                                .map(|qualified| (qualified, option))
                            })
                            .transpose();
                        checks.push(match qualification {
                            Ok(Some((true, option))) => Check {
                                id: format!("mcp_qualification:{server}"),
                                label: format!(
                                    "Connected service “{server}” has retained resource qualification"
                                ),
                                status: CheckStatus::Pass,
                                detail: format!(
                                    "Retained evidence matches the current tool schema digest {} on {}.",
                                    option.tool_schema_digest.as_deref().unwrap_or("missing"),
                                    crate::routes::options::route_label(
                                        runtime,
                                        &model.provider,
                                        &model.deployment
                                    )
                                ),
                            },
                            Ok(Some((false, option))) => Check {
                                id: format!("mcp_qualification:{server}"),
                                label: format!(
                                    "Connected service “{server}” lacks retained resource qualification"
                                ),
                                status: CheckStatus::Fail,
                                detail: format!(
                                    "No retained resource-scoped qualification record matches the current tool schema digest {} on {}. Generic route records do not prove this MCP server.",
                                    option.tool_schema_digest.as_deref().unwrap_or("missing"),
                                    crate::routes::options::route_label(
                                        runtime,
                                        &model.provider,
                                        &model.deployment
                                    )
                                ),
                            },
                            Ok(None) => Check {
                                id: format!("mcp_qualification:{server}"),
                                label: format!(
                                    "Connected service “{server}” could not be matched to live metadata"
                                ),
                                status: CheckStatus::Fail,
                                detail:
                                    "The live MCP catalogue has no current schema digest for this server, so resource-scoped qualification cannot be proven."
                                        .into(),
                            },
                            Err(error) => Check {
                                id: format!("mcp_qualification:{server}"),
                                label: format!(
                                    "Connected service “{server}” qualification could not be evaluated"
                                ),
                                status: CheckStatus::Fail,
                                detail: format!(
                                    "Resource qualification configuration error: {error}"
                                ),
                            },
                        });
                    }
                    if let Some(memory) = bp.memory.as_deref().filter(|memory| !memory.is_empty()) {
                        let qualification = find_resource(&options.memories, memory)
                            .map(|option| {
                                crate::routes::options::memory_binding_qualified_for_route(
                                    runtime,
                                    &model.provider,
                                    &model.deployment,
                                    option,
                                )
                                .map(|qualified| (qualified, option))
                            })
                            .transpose();
                        checks.push(match qualification {
                            Ok(Some((true, option))) => Check {
                                id: "memory_qualification".into(),
                                label: format!(
                                    "Shared memory “{memory}” has retained resource qualification"
                                ),
                                status: CheckStatus::Pass,
                                detail: format!(
                                    "Retained evidence matches backend {} and compiled digest {} on {}.",
                                    option.backend.as_deref().unwrap_or("missing"),
                                    option.compiled_digest.as_deref().unwrap_or("missing"),
                                    crate::routes::options::route_label(
                                        runtime,
                                        &model.provider,
                                        &model.deployment
                                    )
                                ),
                            },
                            Ok(Some((false, option))) => Check {
                                id: "memory_qualification".into(),
                                label: format!(
                                    "Shared memory “{memory}” lacks retained resource qualification"
                                ),
                                status: CheckStatus::Fail,
                                detail: format!(
                                    "No retained resource-scoped qualification record matches backend {} and compiled digest {} on {}. Generic route records do not prove this memory binding.",
                                    option.backend.as_deref().unwrap_or("missing"),
                                    option.compiled_digest.as_deref().unwrap_or("missing"),
                                    crate::routes::options::route_label(
                                        runtime,
                                        &model.provider,
                                        &model.deployment
                                    )
                                ),
                            },
                            Ok(None) => Check {
                                id: "memory_qualification".into(),
                                label: format!(
                                    "Shared memory “{memory}” could not be matched to live metadata"
                                ),
                                status: CheckStatus::Fail,
                                detail:
                                    "The live memory catalogue has no current backend and compiled digest for this binding, so resource-scoped qualification cannot be proven."
                                        .into(),
                            },
                            Err(error) => Check {
                                id: "memory_qualification".into(),
                                label: format!(
                                    "Shared memory “{memory}” qualification could not be evaluated"
                                ),
                                status: CheckStatus::Fail,
                                detail: format!(
                                    "Resource qualification configuration error: {error}"
                                ),
                            },
                        });
                    }
                    for skill in &bp.skills {
                        let qualification = find_resource(&options.skills, skill)
                            .map(|option| {
                                crate::routes::options::skill_version_qualified_for_route(
                                    runtime,
                                    &model.provider,
                                    &model.deployment,
                                    option,
                                )
                                .map(|qualified| (qualified, option))
                            })
                            .transpose();
                        checks.push(match qualification {
                            Ok(Some((true, option))) => Check {
                                id: format!("skill_qualification:{skill}"),
                                label: format!(
                                    "Skill “{skill}” has retained resource qualification"
                                ),
                                status: CheckStatus::Pass,
                                detail: format!(
                                    "Retained evidence matches the current approved version digest {} on {}.",
                                    option.version_digest.as_deref().unwrap_or("missing"),
                                    crate::routes::options::route_label(
                                        runtime,
                                        &model.provider,
                                        &model.deployment
                                    )
                                ),
                            },
                            Ok(Some((false, option))) => Check {
                                id: format!("skill_qualification:{skill}"),
                                label: format!(
                                    "Skill “{skill}” lacks retained resource qualification"
                                ),
                                status: CheckStatus::Fail,
                                detail: format!(
                                    "No retained resource-scoped qualification record matches the current approved version digest {} on {}. Generic route records do not prove this skill version.",
                                    option.version_digest.as_deref().unwrap_or("missing"),
                                    crate::routes::options::route_label(
                                        runtime,
                                        &model.provider,
                                        &model.deployment
                                    )
                                ),
                            },
                            Ok(None) => Check {
                                id: format!("skill_qualification:{skill}"),
                                label: format!(
                                    "Skill “{skill}” could not be matched to live metadata"
                                ),
                                status: CheckStatus::Fail,
                                detail:
                                    "The live skill catalogue has no current approved version digest for this skill, so resource-scoped qualification cannot be proven."
                                        .into(),
                            },
                            Err(error) => Check {
                                id: format!("skill_qualification:{skill}"),
                                label: format!(
                                    "Skill “{skill}” qualification could not be evaluated"
                                ),
                                status: CheckStatus::Fail,
                                detail: format!(
                                    "Resource qualification configuration error: {error}"
                                ),
                            },
                        });
                    }
                }
                let runtime = bp.runtime.as_deref().unwrap_or("OpenClaw");
                let (required_capabilities, max_parallel) =
                    qualification_requirements(bp, workload);
                if bp.model_fallbacks.len() > 8 {
                    checks.push(Check {
                        id: "model_fallback_count".into(),
                        label: "Too many fallback model routes".into(),
                        status: CheckStatus::Fail,
                        detail: "A blueprint may declare at most 8 ordered fallback routes.".into(),
                    });
                }
                for (index, fallback) in bp.model_fallbacks.iter().enumerate() {
                    let route = crate::routes::options::route_label(
                        runtime,
                        &fallback.provider,
                        &fallback.deployment,
                    );
                    let known = options.models.iter().any(|option| {
                        option.provider == fallback.provider
                            && option.deployment == fallback.deployment
                    });
                    let route_qualified = known
                        && crate::routes::options::route_qualification(
                            runtime,
                            &fallback.provider,
                            &fallback.deployment,
                            &required_capabilities,
                            max_parallel,
                            budget_tokens,
                        )
                        .unwrap_or(false);
                    let mcp_qualified = bp.mcp_servers.iter().all(|server| {
                        options
                            .mcp_servers
                            .iter()
                            .find(|option| option.name == *server)
                            .is_some_and(|option| {
                                crate::routes::options::mcp_server_qualified_for_route(
                                    runtime,
                                    &fallback.provider,
                                    &fallback.deployment,
                                    option,
                                )
                                .unwrap_or(false)
                            })
                    });
                    let memory_qualified = bp.memory.as_ref().is_none_or(|memory| {
                        options
                            .memories
                            .iter()
                            .find(|option| option.name == *memory)
                            .is_some_and(|option| {
                                crate::routes::options::memory_binding_qualified_for_route(
                                    runtime,
                                    &fallback.provider,
                                    &fallback.deployment,
                                    option,
                                )
                                .unwrap_or(false)
                            })
                    });
                    let skills_qualified = bp.skills.iter().all(|skill| {
                        options
                            .skills
                            .iter()
                            .find(|option| option.name == *skill)
                            .is_some_and(|option| {
                                crate::routes::options::skill_version_qualified_for_route(
                                    runtime,
                                    &fallback.provider,
                                    &fallback.deployment,
                                    option,
                                )
                                .unwrap_or(false)
                            })
                    });
                    let qualified =
                        route_qualified && mcp_qualified && memory_qualified && skills_qualified;
                    checks.push(Check {
                        id: format!("model_fallback:{index}"),
                        label: if qualified {
                            format!("Fallback {route} is qualified")
                        } else {
                            format!("Fallback {route} is not qualified")
                        },
                        status: if qualified {
                            CheckStatus::Pass
                        } else {
                            CheckStatus::Fail
                        },
                        detail: if qualified {
                            "Retained evidence proves the complete capability and selected-resource contract for this fallback route.".into()
                        } else if !known {
                            "This fallback is absent from the live model catalogue.".into()
                        } else if !route_qualified {
                            "No atomic qualification record proves the complete capability contract for this fallback.".into()
                        } else {
                            "The route is generally qualified, but at least one selected MCP server, memory binding, or skill version lacks current resource-scoped evidence on it.".into()
                        },
                    });
                }
            }
            Err(error) => checks.push(Check {
                id: "model".into(),
                label: "Live model catalogue could not be verified".into(),
                status: CheckStatus::Fail,
                detail: format!(
                    "Pre-flight could not confirm the requested provider/model route: {error}"
                ),
            }),
        }
    }
}
