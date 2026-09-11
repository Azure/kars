// kars Bridge BFF — pre-flight validation gate (design note §20).
//
// The riskiest moment is the handoff from an edited package to a running agent
// with real tools, network, and authority. This endpoint validates the package
// against the LIVE cluster before a single agent starts, and returns an
// itemised pass/fail — never a black-box "go". It checks what can be checked
// honestly from the BFF today: the referenced ToolPolicy / McpServer /
// KarsMemory exist, the model is one the cluster serves, and each egress host
// resolves. Deeper in-sandbox usability probes (a live MCP handshake, a
// tool-invocation probe, RBAC-delegation) are a named next step and are
// reported as such rather than faked.

use std::time::Duration;

use axum::Json;
use axum::extract::{Extension, Path, State};
use serde::{Deserialize, Serialize};

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::routes::ownership::require_owned_task;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct ValidateRequest {
    #[serde(default)]
    pub blueprint: Option<crate::routes::tasks::BlueprintDto>,
    /// The autonomy tier the mission will run at (1..5). Validated so the launch
    /// gate actually covers the envelope the UI shows, not just the blueprint.
    #[serde(default)]
    pub tier: Option<i32>,
    /// The token budget cap, when set. Validated for sanity (positive, not
    /// absurdly small) so a mis-typed cap is caught before launch.
    #[serde(default)]
    pub budget_tokens: Option<i64>,
    /// Structural launch surface. Team composition requires retained Team E2E
    /// evidence in addition to generic mission execution evidence.
    #[serde(default)]
    pub workload: Option<String>,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Fail,
    Warn,
}

#[derive(Debug, Serialize)]
pub struct Check {
    pub id: String,
    pub label: String,
    pub status: CheckStatus,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct ValidateResponse {
    /// True only when there are no failing checks — the launch gate.
    pub ok: bool,
    pub checks: Vec<Check>,
}

fn require_cluster(state: &AppState) -> AppResult<&crate::kars::cluster::Cluster> {
    state.cluster().ok_or(AppError::ClusterUnavailable)
}

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

/// `POST /api/namespaces/:ns/validate` — validate a launch package against live
/// cluster state. Pure read + DNS; never mutates anything.
pub async fn validate_package(
    State(state): State<AppState>,
    Path(ns): Path<String>,
    Json(req): Json<ValidateRequest>,
) -> AppResult<Json<ValidateResponse>> {
    let cluster = require_cluster(&state)?;
    let bp = req.blueprint.unwrap_or_default();
    Ok(Json(
        run_checks(
            cluster,
            &ns,
            &bp,
            req.tier,
            req.budget_tokens,
            req.workload.as_deref(),
        )
        .await,
    ))
}

/// `POST /api/namespaces/:ns/tasks/:name/validate` — validate a task's *own*
/// stored blueprint. This protects the launch gate for a draft created
/// earlier: launching it from the mission detail re-runs the same §20 checks
/// against the package the controller would actually materialize.
pub async fn validate_task(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<ValidateResponse>> {
    let cluster = require_cluster(&state)?;
    let task = require_owned_task(cluster, &ns, &name, &principal).await?;
    let bp = task
        .spec
        .blueprint
        .as_ref()
        .map(blueprint_to_dto)
        .unwrap_or_default();
    let tier = Some(task.spec.envelope.tier);
    let budget_tokens = task.spec.envelope.budget.as_ref().and_then(|b| b.tokens);
    Ok(Json(
        run_checks(cluster, &ns, &bp, tier, budget_tokens, None).await,
    ))
}

/// Project the typed task blueprint into the validation DTO (the checks operate
/// on the same shape the create path accepts).
fn blueprint_to_dto(b: &crate::kars::task::TaskBlueprint) -> crate::routes::tasks::BlueprintDto {
    use crate::routes::tasks::{BlueprintDto, EgressDto, ModelDto};
    BlueprintDto {
        runtime: b.runtime.clone(),
        model: b.model.as_ref().map(|m| ModelDto {
            provider: m.provider.clone(),
            deployment: m.deployment.clone(),
        }),
        model_fallbacks: b
            .model_fallbacks
            .iter()
            .map(|m| ModelDto {
                provider: m.provider.clone(),
                deployment: m.deployment.clone(),
            })
            .collect(),
        instructions: b.instructions.clone(),
        tool_policy: b.tool_policy.clone(),
        mcp_servers: b.mcp_servers.clone(),
        egress: b
            .egress
            .iter()
            .map(|e| EgressDto {
                host: e.host.clone(),
                port: e.port,
            })
            .collect(),
        egress_mode: b.egress_mode.clone(),
        isolation: b.isolation.clone(),
        memory: b.memory.clone(),
        skills: b.skills.clone(),
        execution_plan: b
            .execution_plan
            .as_ref()
            .map(crate::routes::tasks::ExecutionPlanDto::from_crd),
    }
}

/// Run the full §20 check suite against a package. Pure read + DNS.
async fn run_checks(
    cluster: &crate::kars::cluster::Cluster,
    namespace: &str,
    bp: &crate::routes::tasks::BlueprintDto,
    tier: Option<i32>,
    budget_tokens: Option<i64>,
    workload: Option<&str>,
) -> ValidateResponse {
    let mut checks: Vec<Check> = Vec::new();

    // 0. Envelope sanity — the autonomy tier and budget the operator will launch
    //    with. The UI presents validation as covering the whole package, so the
    //    gate must actually check the envelope, not only the blueprint.
    if let Some(t) = tier {
        checks.push(if (1..=5).contains(&t) {
            Check {
                id: "tier".into(),
                label: format!("Autonomy tier {t}"),
                status: CheckStatus::Pass,
                detail: "Within the valid range (1–5). Sub-roles are capped one tier below.".into(),
            }
        } else {
            Check {
                id: "tier".into(),
                label: "Autonomy tier".into(),
                status: CheckStatus::Fail,
                detail: format!("Tier {t} is out of range — must be 1–5."),
            }
        });
    }
    let runtime = bp.runtime.as_deref().unwrap_or("OpenClaw");
    match budget_tokens {
        Some(b) if b <= 0 => checks.push(Check {
            id: "budget".into(),
            label: "Token budget".into(),
            status: CheckStatus::Fail,
            detail: "The budget cap must be a positive number of tokens.".into(),
        }),
        Some(b) if b < 500 => checks.push(Check {
            id: "budget".into(),
            label: format!("Token budget {b}"),
            status: CheckStatus::Warn,
            detail: "This cap is very low — a real run may stop before producing a deliverable.".into(),
        }),
        Some(b) => checks.push(Check {
            id: "budget".into(),
            label: format!("Token budget {b}"),
            status: CheckStatus::Pass,
            detail: "A per-run token cap is set — the router stops the run at this ceiling.".into(),
        }),
        None => checks.push(Check {
            id: "budget".into(),
            label: "Token budget".into(),
            status: CheckStatus::Warn,
            detail: "No token cap set — the run is bounded only by the mission's autonomy and the cluster defaults.".into(),
        }),
    }
    if let Some(plan) = bp.execution_plan.as_ref() {
        checks.push(match crate::routes::compose::validate_execution_plan(plan) {
            Ok(()) => Check {
                id: "execution_plan".into(),
                label: format!(
                    "Execution plan · {} role{} · up to {} in parallel",
                    plan.roles.len(),
                    if plan.roles.len() == 1 { "" } else { "s" },
                    plan.max_parallel
                ),
                status: CheckStatus::Pass,
                detail:
                    "Role dependencies, phases, capabilities, tool-call bounds, synthesis, and deliverables are valid."
                        .into(),
            },
            Err(error) => Check {
                id: "execution_plan".into(),
                label: "Execution plan is invalid".into(),
                status: CheckStatus::Fail,
                detail: error,
            },
        });
    }

    // 0b. Runtime image and registry credentials. This is a real launch blocker:
    // allowing a configured private image without a matching pull secret produces
    // an immediate ImagePullBackOff before the agent can execute anything.
    let runnable_runtimes = cluster.runnable_runtimes().await;
    checks.push(if runnable_runtimes.contains(runtime) {
        Check {
            id: "runtime".into(),
            label: format!("Runtime “{runtime}” can start on this cluster"),
            status: CheckStatus::Pass,
            detail: "The runtime image is configured and its registry is covered by the controller's pull credentials.".into(),
        }
    } else {
        Check {
            id: "runtime".into(),
            label: format!("Runtime “{runtime}” cannot start on this cluster"),
            status: CheckStatus::Fail,
            detail: "The runtime image is missing or its private registry is not covered by the controller's pull credentials. Launch is blocked to prevent ImagePullBackOff.".into(),
        }
    });
    if runtime != "OpenClaw" && !bp.skills.is_empty() {
        checks.push(Check {
            id: "runtime_skills".into(),
            label: format!("Runtime “{runtime}” cannot mount file skills"),
            status: CheckStatus::Fail,
            detail: "Controller-mounted file skills are currently supported only by OpenClaw; remove them or use OpenClaw instead.".into(),
        });
    }

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

    // 5. Egress hosts resolve (DNS) — a real, honest reachability signal from
    //    the BFF (not the full in-sandbox egress path, which is a deeper probe).
    for e in &bp.egress {
        let port = e.port.unwrap_or(443) as u16;
        let resolved = resolves(&e.host, port).await;
        checks.push(Check {
            id: format!("egress:{}", e.host),
            label: if resolved {
                format!("Egress host {} resolves", e.host)
            } else {
                format!("Egress host {} does not resolve", e.host)
            },
            status: if resolved { CheckStatus::Pass } else { CheckStatus::Warn },
            detail: if resolved {
                "The host resolves in DNS. Full reachability from the sandbox egress path is a deeper probe (named next step).".into()
            } else {
                "The host did not resolve from the gateway. Check the spelling; it may still be reachable from inside the cluster.".into()
            },
        });
    }

    if checks.is_empty() {
        checks.push(Check {
            id: "baseline".into(),
            label: "Package is launch-ready".into(),
            status: CheckStatus::Pass,
            detail: "No external tools, services, memory, or custom egress to validate — the mission runs on the model alone within its envelope.".into(),
        });
    }

    let ok = !checks.iter().any(|c| c.status == CheckStatus::Fail);
    ValidateResponse { ok, checks }
}

/// Extract the host from a URL string for a reachability check. Best-effort:
/// strips a scheme and any path/port. Returns `None` for an empty host.
fn url_host(url: &str) -> Option<String> {
    let after_scheme = url.split("://").nth(1).unwrap_or(url);
    let host = after_scheme
        .split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// True when `host:port` resolves in DNS within a short timeout. A real,
/// honest reachability signal from the gateway — not a full connection.
async fn resolves(host: &str, port: u16) -> bool {
    let addr = format!("{host}:{port}");
    tokio::time::timeout(Duration::from_secs(3), tokio::net::lookup_host(&addr))
        .await
        .ok()
        .and_then(|r| r.ok())
        .map(|mut it| it.next().is_some())
        .unwrap_or(false)
}

#[cfg(test)]
mod qualification_requirement_tests {
    use super::{blueprint_to_dto, qualification_requirements};

    #[test]
    fn persisted_draft_plan_preserves_create_preflight_requirements_and_all_plan_fields() {
        use crate::kars::task::TaskBlueprint;
        use crate::routes::tasks::{BlueprintDto, ExecutionPlanDto};
        let plan: ExecutionPlanDto = serde_json::from_value(serde_json::json!({
            "schema": "kars.execution-plan/v1",
            "roles": [
                {
                    "name": "evidence",
                    "objective": "Read public evidence.",
                    "depends_on": [],
                    "phases": [{
                        "name": "inspect",
                        "objective": "Read current check logs.",
                        "capabilities": ["network", "mcp"],
                        "required_tool_calls": [{
                            "name": "github_actions_job_logs",
                            "arguments": {"owner": "owner", "repo": "repo", "job_id": "42"}
                        }],
                        "min_tool_calls": 1,
                        "max_tool_calls": 4,
                        "fresh_context": true
                    }],
                    "budget_tokens": 1500
                },
                {
                    "name": "review",
                    "objective": "Review the evidence.",
                    "depends_on": ["evidence"],
                    "phases": [{
                        "name": "assess",
                        "objective": "Assess the handback.",
                        "capabilities": [],
                        "required_tool_calls": [],
                        "min_tool_calls": 0,
                        "max_tool_calls": 0,
                        "fresh_context": false
                    }],
                    "budget_tokens": 500
                }
            ],
            "max_parallel": 2,
            "synthesis": {
                "objective": "Produce the review.",
                "capabilities": ["network"],
                "max_tool_calls": 1
            },
            "deliverables": [
                {"name": "review.md", "media_type": "text/markdown"},
                {"name": "evidence.json", "media_type": null}
            ]
        }))
        .unwrap();
        let create = BlueprintDto {
            execution_plan: Some(plan.clone()),
            ..Default::default()
        };
        let stored = serde_json::to_value(TaskBlueprint {
            execution_plan: Some(plan.into_crd()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(stored["executionPlan"]["maxParallel"], 2);
        let persisted: TaskBlueprint = serde_json::from_value(stored).unwrap();
        let launch = blueprint_to_dto(&persisted);
        assert_eq!(
            serde_json::to_value(&launch.execution_plan).unwrap(),
            serde_json::to_value(&create.execution_plan).unwrap()
        );
        let required = qualification_requirements(&launch, Some("mission"));
        assert_eq!(
            required,
            qualification_requirements(&create, Some("mission"))
        );
        assert_eq!(required.1, 2);
        for capability in ["artifacts", "delegation", "mcp", "network", "telemetry"] {
            assert!(required.0.contains(capability));
        }
        assert!(!required.0.contains("single-agent"));
    }

    #[test]
    fn legacy_draft_without_a_plan_still_requires_single_agent_qualification() {
        let launch = blueprint_to_dto(&crate::kars::task::TaskBlueprint::default());
        assert!(launch.execution_plan.is_none());
        let (required, parallel) = qualification_requirements(&launch, Some("mission"));
        assert_eq!(parallel, 1);
        assert!(required.contains("single-agent"));
        assert!(!required.contains("delegation"));
    }

    #[test]
    fn team_preflight_requires_retained_team_evidence() {
        let blueprint = crate::routes::tasks::BlueprintDto::default();
        let (mission, _) = qualification_requirements(&blueprint, Some("mission"));
        let (team, _) = qualification_requirements(&blueprint, Some("team"));

        assert!(!mission.contains("team"));
        assert!(team.contains("team"));
    }

    #[test]
    fn execution_plan_capabilities_flow_into_atomic_qualification_requirements() {
        let blueprint = crate::routes::tasks::BlueprintDto {
            execution_plan: Some(crate::routes::tasks::ExecutionPlanDto {
                schema: "kars.execution-plan/v1".into(),
                roles: vec![crate::routes::tasks::ExecutionRoleDto {
                    name: "source-scout".into(),
                    objective: "Discover exact URLs and fetch the source evidence.".into(),
                    depends_on: Vec::new(),
                    phases: vec![crate::routes::tasks::ExecutionPhaseDto {
                        name: "discover".into(),
                        objective: "Search and fetch authoritative sources.".into(),
                        capabilities: vec!["web-search".into(), "network".into()],
                        required_tool_calls: Vec::new(),
                        min_tool_calls: 1,
                        max_tool_calls: 4,
                        fresh_context: true,
                    }],
                    budget_tokens: None,
                }],
                max_parallel: 1,
                synthesis: crate::routes::tasks::ExecutionSynthesisDto {
                    objective: "Return the verified answer.".into(),
                    capabilities: Vec::new(),
                    max_tool_calls: 0,
                },
                deliverables: Vec::new(),
            }),
            ..Default::default()
        };

        let (required, max_parallel) = qualification_requirements(&blueprint, Some("mission"));
        assert_eq!(max_parallel, 1);
        assert!(required.contains("delegation"));
        assert!(required.contains("web-search"));
        assert!(required.contains("network"));
    }
}
