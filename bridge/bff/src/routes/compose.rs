// kars Bridge BFF — the launch-package orchestrator (§20 "intent → package").
//
// Turns a plain-language objective into a *proposed*, fully-governed launch
// package — the trust envelope (autonomy tier, budget), the model, harness,
// standing instructions, tool policy, connected MCP servers, network egress,
// isolation, and shared memory — composed by an LLM that is constrained to the
// REAL building blocks this cluster offers (from `/api/options`).
//
// Honesty + safety:
// - This is a PROPOSAL, not an action. Nothing is provisioned. The operator
//   reviews and edits every field, then the §20 pre-flight validation gate and
//   the explicit Launch step still govern what actually runs.
// - The LLM may only reference building blocks that exist; the server
//   re-validates the proposal against the live options and drops/normalizes
//   anything that doesn't, so the orchestrator can never invent a model, tool
//   policy, MCP server, or isolation level the cluster can't honor.
// - The orchestrator endpoint + credentials are BFF-side config. When they are
//   not set the endpoint reports `available: false` and the UI falls back to the
//   manual composer — never a fabricated package.

use axum::{
    Json,
    extract::{Extension, Path, State},
};
use serde::{Deserialize, Serialize};

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::routes::options::ModelOption;
use crate::routes::options::build_options;
use crate::state::AppState;

/// The cluster-default governance policy. Assigned to any envelope the
/// orchestrator (or operator) leaves un-governed, so the sandbox is BOTH
/// governed and functional: the agent runtime always initializes its AGT
/// engine and fails closed on an empty policy set, so an un-governed envelope
/// otherwise yields a sandbox that hangs (no tool/inference/mesh permitted).
/// `kars-default` allows inference/tool/mesh/spawn and denies dangerous shell.
pub const DEFAULT_TOOL_POLICY: &str = "kars-default";
const MISSION_COMPOSE_MAX_TOKENS: u32 = 4_096;
const LOOP_COMPOSE_MAX_TOKENS: u32 = 1_200;
// A team proposal can contain eight milestone contracts plus four role contracts.
// Keep enough output room for the model's complete JSON rather than accepting a
// syntactically truncated proposal and wasting the single repair attempt.
const TEAM_COMPOSE_MAX_TOKENS: u32 = 8_192;

/// Resolve a governance policy for an envelope the orchestrator left
/// un-governed: prefer `kars-default` when the cluster has it, otherwise the
/// first installed policy. Returns `None` only when the cluster has no policies
/// at all (nothing to assign).
pub fn default_tool_policy(o: &crate::routes::options::Options) -> Option<String> {
    if o.tool_policies
        .iter()
        .any(|tp| tp.name == DEFAULT_TOOL_POLICY)
    {
        return Some(DEFAULT_TOOL_POLICY.to_string());
    }
    o.tool_policies.first().map(|tp| tp.name.clone())
}

fn orchestrator_quality_score(deployment: &str) -> Option<i64> {
    let model = deployment.to_ascii_lowercase().replace(['.', '_'], "-");
    if model.contains("embedding")
        || model.contains("image")
        || model.contains("flux")
        || model.contains("dall-e")
    {
        return None;
    }
    let score = if model.contains("gpt-5-6") || model.contains("gpt-5.6") {
        1_000
    } else if model.contains("claude-opus-4-8") {
        990
    } else if model.contains("claude-opus-4-7") {
        980
    } else if model.contains("gpt-5-4-pro") {
        970
    } else if model.contains("gpt-5-4") {
        950
    } else if model.contains("claude-sonnet-5") {
        940
    } else if model.contains("gpt-4-1") {
        900
    } else if model.contains("gpt-oss-120b") {
        850
    } else if model.contains("gpt-5") || model.contains("claude") {
        800
    } else {
        500
    };
    Some(score)
}

fn catalogue_has_model(models: &[ModelOption], provider: &str, deployment: &str) -> bool {
    models
        .iter()
        .any(|model| model.provider == provider && model.deployment == deployment)
}

fn catalogue_has_model_key(models: &[ModelOption], key: &str) -> bool {
    key.split_once("::")
        .is_some_and(|(provider, deployment)| catalogue_has_model(models, provider, deployment))
}

fn recommendation_is_actionable(recommended: Option<&str>, low_confidence: bool) -> bool {
    recommended.is_some() && !low_confidence
}

fn select_orchestrator_route(
    options: &crate::routes::options::Options,
    efficiency: &crate::routes::efficiency::EfficiencyDto,
) -> Option<(String, String, String)> {
    let actionable_recommendation = efficiency.recommended.as_deref().filter(|_| {
        recommendation_is_actionable(
            efficiency.recommended.as_deref(),
            efficiency.recommended_low_confidence,
        )
    });
    options
            .models
            .iter()
            .filter_map(|model| {
                let quality = orchestrator_quality_score(&model.deployment)?;
                let route = efficiency
                    .routes
                    .iter()
                    .find(|route| route.route == model.deployment);
                let frontier_bonus = if actionable_recommendation == Some(model.deployment.as_str()) {
                    80
                } else {
                    0
                };
                let evidence_bonus = route
                    .map(|route| (route.acceptance_rate * 50.0).round() as i64)
                    .unwrap_or(0);
                Some((quality + frontier_bonus + evidence_bonus, model))
            })
            .max_by_key(|(score, _)| *score)
            .map(|(_, model)| {
                let basis = if actionable_recommendation == Some(model.deployment.as_str()) {
                    format!(
                        "Selected {} from {} as the strongest orchestration-capable model and current efficiency-frontier recommendation.",
                        model.deployment, model.provider
                    )
                } else {
                    format!(
                        "Selected {} from {} as the strongest orchestration-capable model in the configured catalogue.",
                        model.deployment, model.provider
                    )
                };
                (model.provider.clone(), model.deployment.clone(), basis)
            })
}

#[derive(Debug, Deserialize)]
pub struct ComposeRequest {
    pub objective: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComposeModel {
    pub provider: String,
    pub deployment: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComposeEgress {
    pub host: String,
    pub port: Option<u16>,
}

fn complete_egress_recommendation(
    egress: Vec<ComposeEgress>,
    intent: &str,
    mcp_servers: &[String],
) -> Vec<ComposeEgress> {
    let mut endpoints = std::collections::BTreeMap::<String, Option<u16>>::new();
    let lower = intent.to_ascii_lowercase();
    let npm_intent = ["npm", "node", "javascript", "typescript", "package.json"]
        .iter()
        .any(|term| lower.contains(term));
    let python_intent = ["python", "pip", "pypi", "requirements.txt"]
        .iter()
        .any(|term| lower.contains(term));
    for endpoint in egress {
        let host = endpoint.host.to_ascii_lowercase();
        if host.contains("githubcopilot.com")
            || host.ends_with(".openai.azure.com")
            || host.ends_with(".services.ai.azure.com")
        {
            continue;
        }
        if host == "registry.npmjs.org" && !npm_intent {
            continue;
        }
        if matches!(host.as_str(), "pypi.org" | "files.pythonhosted.org") && !python_intent {
            continue;
        }
        endpoints.insert(host, endpoint.port.or(Some(443)));
    }
    let github = mcp_servers
        .iter()
        .any(|server| server.to_ascii_lowercase().contains("github"))
        || [
            "github",
            "repository",
            "pull request",
            "dependabot",
            "code scanning",
        ]
        .iter()
        .any(|term| lower.contains(term));
    let mut add = |host: &str| {
        endpoints.entry(host.to_string()).or_insert(Some(443));
    };
    if github {
        for host in [
            "api.github.com",
            "github.com",
            "raw.githubusercontent.com",
            "codeload.github.com",
            "objects.githubusercontent.com",
            "patch-diff.githubusercontent.com",
        ] {
            add(host);
        }
    }
    if npm_intent {
        add("registry.npmjs.org");
    }
    if python_intent {
        add("pypi.org");
        add("files.pythonhosted.org");
    }
    if [
        "security advisory",
        "security advisories",
        "vulnerability",
        "vulnerabilities",
        "cve",
    ]
    .iter()
    .any(|term| lower.contains(term))
    {
        add("api.osv.dev");
    }
    if ["rust", "cargo", "crates.io"]
        .iter()
        .any(|term| lower.contains(term))
    {
        add("index.crates.io");
        add("static.crates.io");
    }
    endpoints
        .into_iter()
        .map(|(host, port)| ComposeEgress { host, port })
        .collect()
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ComposeDelegationRole {
    pub name: String,
    pub objective: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ComposeDelegation {
    pub mode: String,
    pub roles: Vec<ComposeDelegationRole>,
    pub max_parallel: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComposeProposal {
    pub tier: i32,
    pub model: Option<ComposeModel>,
    /// Ordered routes that independently qualify the complete Mission package.
    pub model_fallbacks: Vec<ComposeModel>,
    /// Plain-language basis for the model choice, so the reviewer sees WHY this
    /// model was proposed — the learned efficiency frontier, the orchestrator's
    /// objective-driven pick, or the cluster default. Never fabricated.
    pub model_basis: Option<String>,
    pub runtime: String,
    pub instructions: String,
    pub tool_policy: Option<String>,
    pub mcp_servers: Vec<String>,
    pub skills: Vec<String>,
    pub egress: Vec<ComposeEgress>,
    pub isolation: String,
    pub memory: Option<String>,
    pub budget_tokens: Option<i64>,
    pub execution_plan: Option<crate::routes::tasks::ExecutionPlanDto>,
    pub delegation: ComposeDelegation,
}

#[derive(Debug, Serialize)]
pub struct ComposeResponse {
    /// Whether the orchestrator is configured + reachable on this deployment.
    pub available: bool,
    /// Why it isn't available (only set when `available` is false), so the UI
    /// can explain the manual-composer fallback honestly.
    pub reason: Option<String>,
    /// The composed, validated launch package — ready to review and edit.
    pub proposal: Option<ComposeProposal>,
    /// A short, plain-language rationale for the choices (for the reviewer).
    pub rationale: Option<String>,
    /// The model that composed the package (provenance).
    pub source: Option<String>,
}

fn mission_proposal_blueprint(
    proposal: &ComposeProposal,
) -> Result<
    (
        crate::routes::tasks::BlueprintDto,
        &ComposeModel,
        std::collections::BTreeSet<String>,
        i32,
    ),
    String,
> {
    let model = proposal
        .model
        .as_ref()
        .ok_or_else(|| "the proposal has no model route".to_string())?;
    let blueprint = crate::routes::tasks::BlueprintDto {
        runtime: Some(proposal.runtime.clone()),
        model: Some(crate::routes::tasks::ModelDto {
            provider: model.provider.clone(),
            deployment: model.deployment.clone(),
        }),
        model_fallbacks: proposal
            .model_fallbacks
            .iter()
            .map(|model| crate::routes::tasks::ModelDto {
                provider: model.provider.clone(),
                deployment: model.deployment.clone(),
            })
            .collect(),
        instructions: Some(proposal.instructions.clone()),
        tool_policy: proposal.tool_policy.clone(),
        mcp_servers: proposal.mcp_servers.clone(),
        egress: proposal
            .egress
            .iter()
            .map(|endpoint| crate::routes::tasks::EgressDto {
                host: endpoint.host.clone(),
                port: endpoint.port.map(i32::from),
            })
            .collect(),
        egress_mode: Some("strict".into()),
        isolation: Some(proposal.isolation.clone()),
        memory: proposal.memory.clone(),
        skills: proposal.skills.clone(),
        execution_plan: proposal.execution_plan.clone(),
    };
    let (required, max_parallel) =
        crate::routes::validate::qualification_requirements(&blueprint, None);
    Ok((blueprint, model, required, max_parallel))
}

fn apply_mission_budget_floor(proposal: &mut ComposeProposal) -> Result<Option<i64>, String> {
    let (_, model, required, max_parallel) = mission_proposal_blueprint(proposal)?;
    let minimum = crate::routes::options::route_minimum_tokens(
        &proposal.runtime,
        &model.provider,
        &model.deployment,
        &required,
        max_parallel,
    )?;
    let Some(minimum) = minimum else {
        return Ok(None);
    };

    let mut changed = false;
    let mut required_total = minimum;
    if let Some(plan) = proposal.execution_plan.as_mut()
        && !plan.roles.is_empty()
    {
        let (role_budgets_changed, role_budget_total) =
            apply_weighted_role_budget_floors(plan, minimum.max(plan.roles.len() as i64));
        changed |= role_budgets_changed;
        required_total = required_total.max(role_budget_total);
    }
    if proposal
        .budget_tokens
        .is_none_or(|current| current < required_total)
    {
        proposal.budget_tokens = Some(required_total);
        changed = true;
    }
    Ok(changed.then_some(required_total))
}

fn mission_proposal_qualification(proposal: &ComposeProposal) -> Result<(), String> {
    let (_, model, required, max_parallel) = mission_proposal_blueprint(proposal)?;
    let qualified = crate::routes::options::route_qualification(
        &proposal.runtime,
        &model.provider,
        &model.deployment,
        &required,
        max_parallel,
        proposal.budget_tokens,
    )?;
    if qualified {
        return Ok(());
    }
    let missing = crate::routes::options::route_qualification_gap(
        &proposal.runtime,
        &model.provider,
        &model.deployment,
        &required,
        max_parallel,
        proposal.budget_tokens,
    )?;
    Err(format!(
        "{} · {}::{} lacks retained qualification for [{}] at max_parallel={max_parallel}",
        proposal.runtime,
        model.provider,
        model.deployment,
        missing.into_iter().collect::<Vec<_>>().join(", "),
    ))
}

fn option_named<'a>(
    options: &'a [crate::routes::options::RefOption],
    name: &str,
) -> Option<&'a crate::routes::options::RefOption> {
    options.iter().find(|option| option.name == name)
}

fn mission_resource_qualification(
    proposal: &ComposeProposal,
    options: &crate::routes::options::Options,
) -> Result<(), String> {
    let (_, model, _, _) = mission_proposal_blueprint(proposal)?;
    let route =
        crate::routes::options::route_label(&proposal.runtime, &model.provider, &model.deployment);
    for server in &proposal.mcp_servers {
        let option = option_named(&options.mcp_servers, server)
            .ok_or_else(|| format!("MCP server `{server}` is not in the live options catalogue"))?;
        if !crate::routes::options::mcp_server_qualified_for_route(
            &proposal.runtime,
            &model.provider,
            &model.deployment,
            option,
        )? {
            return Err(format!(
                "MCP server `{server}` lacks retained resource qualification for {route} at current schema {}. Generic route records do not prove this server.",
                option.tool_schema_digest.as_deref().unwrap_or("missing"),
            ));
        }
    }
    if let Some(memory) = proposal.memory.as_deref() {
        let option = option_named(&options.memories, memory)
            .ok_or_else(|| format!("memory `{memory}` is not in the live options catalogue"))?;
        if !crate::routes::options::memory_binding_qualified_for_route(
            &proposal.runtime,
            &model.provider,
            &model.deployment,
            option,
        )? {
            return Err(format!(
                "Memory `{memory}` lacks retained resource qualification for {route} at backend {} / compiled digest {}. Generic route records do not prove this binding.",
                option.backend.as_deref().unwrap_or("missing"),
                option.compiled_digest.as_deref().unwrap_or("missing"),
            ));
        }
    }
    for skill in &proposal.skills {
        let option = option_named(&options.skills, skill)
            .ok_or_else(|| format!("skill `{skill}` is not in the approved live catalogue"))?;
        if !crate::routes::options::skill_version_qualified_for_route(
            &proposal.runtime,
            &model.provider,
            &model.deployment,
            option,
        )? {
            return Err(format!(
                "Skill `{skill}` lacks retained resource qualification for {route} at current version digest {}. Generic route records do not prove this approved version.",
                option.version_digest.as_deref().unwrap_or("missing"),
            ));
        }
    }
    Ok(())
}

fn mission_proposal_launchability(
    proposal: &ComposeProposal,
    options: &crate::routes::options::Options,
) -> Result<(), String> {
    mission_proposal_qualification(proposal)?;
    mission_resource_qualification(proposal, options)
}

/// `POST /api/namespaces/:ns/compose` — orchestrate a launch package from an
/// objective. The `ns` is accepted for symmetry with the other task routes but
/// composition reads cluster-wide building blocks.
pub async fn compose(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<ComposeRequest>,
) -> AppResult<Json<ComposeResponse>> {
    let cluster = state.cluster().ok_or(AppError::ClusterUnavailable)?;

    let objective = req.objective.trim();
    if objective.is_empty() {
        return Err(AppError::BadRequest("objective is required".into()));
    }

    let options = build_options(cluster).await?;
    let efficiency = crate::routes::efficiency::compute_efficiency_for_owner(
        cluster,
        Some(principal.sub.as_str()),
    )
    .await;
    let orchestrator_route = select_orchestrator_route(&options, &efficiency);

    let qualification_constraints = crate::routes::options::qualification_constraints_summary()
        .unwrap_or_else(|error| format!("  (qualification records unavailable: {error})"));
    let resource_qualification_constraints =
        crate::routes::options::resource_qualification_summary(&options).unwrap_or_else(|error| {
            format!("  (resource qualification records unavailable: {error})")
        });
    let system = build_system_prompt(
        &options,
        &efficiency,
        &qualification_constraints,
        &resource_qualification_constraints,
    );
    let user = format!(
        "Objective:\n{objective}\n\nCompose the launch package now. Respond with ONLY the JSON object."
    );

    // Resolve the model used to CALL the orchestrator. An explicit
    // BRIDGE_ORCHESTRATOR_* env triple overrides (and carries its own model), so
    // it makes `default_model` irrelevant. Otherwise we need a real cluster
    // model — never a fabricated one, which would fail opaquely on a non-Anthropic
    // cluster. When there is neither, say so honestly instead of guessing.
    let env_orchestrator = std::env::var("BRIDGE_ORCHESTRATOR_ENDPOINT")
        .is_ok_and(|v| !v.trim().is_empty())
        && std::env::var("BRIDGE_ORCHESTRATOR_TOKEN").is_ok_and(|v| !v.trim().is_empty())
        && std::env::var("BRIDGE_ORCHESTRATOR_MODEL").is_ok_and(|v| !v.trim().is_empty());
    let pinned_orchestrator_model = cluster.bridge_orchestrator_model().await;
    let resolved_model = orchestrator_route
        .as_ref()
        .map(|(_, deployment, _)| deployment.clone())
        .or(pinned_orchestrator_model)
        .or_else(|| {
            options
                .models
                .iter()
                .find(|m| m.is_default)
                .or_else(|| options.models.first())
                .map(|m| m.deployment.clone())
        });
    if resolved_model.is_none() && !env_orchestrator {
        return Ok(Json(ComposeResponse {
            available: false,
            reason: Some(
                "No inference models are configured on this cluster, so the AI composer can't run. Add an inference provider in the Operator Console, or compose the package manually below.".into(),
            ),
            proposal: None,
            rationale: None,
            source: None,
        }));
    }
    let default_model = resolved_model.unwrap_or_default();
    if !env_orchestrator
        && let Some((provider, deployment, _)) = &orchestrator_route
        && let Err(error) = cluster
            .configure_bridge_orchestrator_model(provider, deployment)
            .await
    {
        return Ok(Json(ComposeResponse {
            available: false,
            reason: Some(format!(
                "The best orchestrator route ({provider}/{deployment}) could not be configured: {error}"
            )),
            proposal: None,
            rationale: None,
            source: None,
        }));
    }

    let (raw, mut source) = match orchestrator_complete(
        cluster,
        &system,
        &user,
        &default_model,
        MISSION_COMPOSE_MAX_TOKENS,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            // A reachable-but-failing orchestrator is reported honestly, not
            // papered over with a fabricated package.
            return Ok(Json(ComposeResponse {
                available: false,
                reason: Some(format!(
                    "The orchestrator could not compose a package ({e}). Compose it manually below — every field is the same one the orchestrator would propose."
                )),
                proposal: None,
                rationale: None,
                source: None,
            }));
        }
    };

    let (mut proposal, mut rationale) = parse_and_validate(&raw, &options, &efficiency, objective);
    if let Ok(Some(minimum)) = apply_mission_budget_floor(&mut proposal) {
        let note = format!(
            "The token budget was raised to the retained qualification floor of {minimum} tokens."
        );
        rationale = Some(match rationale {
            Some(existing) if !existing.trim().is_empty() => format!("{existing} {note}"),
            _ => note,
        });
    }
    if let Err(error) = mission_proposal_launchability(&proposal, &options) {
        let repair_user = format!(
            "{user}\n\nYour previous proposal was not launchable: {error}\n\
             Recompose it so the complete runtime/model/capability/max_parallel requirement fits \
             ONE qualified execution record below. Qualification records do not compose. If a \
             selected MCP server, memory binding, or approved skill is used, it MUST have a \
             retained resource-scoped qualification record at the CURRENT digest on the chosen \
             route — generic route records do not count. If a requested binary or file-writing \
             deliverable needs an unqualified capability, choose a launchable text/JSON \
             alternative and represent diagrams inline with quoted Mermaid flowchart labels \
             whenever they contain parser-sensitive punctuation.\n\n\
             QUALIFIED EXECUTION RECORDS:\n{qualification_constraints}\n\n\
             RESOURCE QUALIFICATION RECORDS:\n{resource_qualification_constraints}\n\n\
             Return ONLY the complete JSON object."
        );
        if let Ok((repair_raw, repair_source)) = orchestrator_complete(
            cluster,
            &system,
            &repair_user,
            &default_model,
            MISSION_COMPOSE_MAX_TOKENS,
        )
        .await
        {
            (proposal, rationale) =
                parse_and_validate(&repair_raw, &options, &efficiency, objective);
            let _ = apply_mission_budget_floor(&mut proposal);
            source = repair_source;
        }
    }
    if let Err(error) = mission_proposal_launchability(&proposal, &options) {
        return Ok(Json(ComposeResponse {
            available: false,
            reason: Some(format!(
                "The orchestrator could not produce a launchable package after retry: {error}"
            )),
            proposal: None,
            rationale: None,
            source: Some(source),
        }));
    }

    proposal.model_fallbacks = qualified_mission_fallbacks(&proposal, &options);
    Ok(Json(ComposeResponse {
        available: true,
        reason: None,
        proposal: Some(proposal),
        rationale,
        source: Some(source),
    }))
}

fn qualified_mission_fallbacks(
    proposal: &ComposeProposal,
    options: &crate::routes::options::Options,
) -> Vec<ComposeModel> {
    let primary = proposal
        .model
        .as_ref()
        .map(|model| format!("{}::{}", model.provider, model.deployment));
    let mut candidates = options
        .models
        .iter()
        .map(|model| (model.provider.clone(), model.deployment.clone()))
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.dedup();
    candidates
        .into_iter()
        .filter(|(provider, deployment)| {
            primary.as_deref() != Some(format!("{provider}::{deployment}").as_str())
        })
        .filter_map(|(provider, deployment)| {
            let mut trial = proposal.clone();
            trial.model = Some(ComposeModel {
                provider: provider.clone(),
                deployment: deployment.clone(),
            });
            trial.model_fallbacks.clear();
            mission_proposal_launchability(&trial, options)
                .is_ok()
                .then_some(ComposeModel {
                    provider,
                    deployment,
                })
        })
        .take(8)
        .collect()
}

/// `POST /api/namespaces/:ns/propose-loop` — the orchestrator turns a raw intent
/// into a PROPOSED loop (2026 loop engineering): it picks the feedback-loop
/// pattern that fits and drafts the goal + success criteria. The web then shows
/// this in the Loop Designer for the user to REVIEW and tweak before executing —
/// so the loop is orchestrator-defined, human-reviewed, then run. Falls back to a
/// keyword heuristic when the orchestrator is unreachable (never a dead end).
#[derive(Debug, serde::Deserialize)]
pub struct ProposeLoopRequest {
    pub intent: String,
    /// "mission" (single run) or "team" (standing cadence loop).
    #[serde(default)]
    pub surface: String,
}

#[derive(Debug, Serialize)]
pub struct ProposeLoopResponse {
    /// Chosen loop pattern id (matches the web catalog: react, reflect,
    /// plan-execute, eval-iterate, explore-branch, standing-watch).
    pub pattern: String,
    /// The goal the orchestrator distilled from the intent.
    pub goal: String,
    /// Draft success criteria (one per line).
    pub criteria: String,
    /// One-line why-this-pattern rationale.
    pub rationale: String,
    /// "orchestrator" when the model chose it, "heuristic" on fallback.
    pub source: String,
}

const LOOP_PATTERN_IDS: [&str; 6] = [
    "react",
    "reflect",
    "plan-execute",
    "eval-iterate",
    "explore-branch",
    "standing-watch",
];

/// Keyword heuristic used both to seed the orchestrator and as the fallback.
fn heuristic_pattern(intent: &str, surface: &str) -> &'static str {
    let t = intent.to_ascii_lowercase();
    if surface == "team"
        || t.contains("watch")
        || t.contains("monitor")
        || t.contains("keep an eye")
        || t.contains("on cadence")
        || t.contains("every ")
    {
        return "standing-watch";
    }
    if t.contains("test")
        || t.contains("verify")
        || t.contains("pass")
        || t.contains("acceptance")
        || t.contains("ci")
    {
        return "eval-iterate";
    }
    if t.contains("research")
        || t.contains("investigate")
        || t.contains("browse")
        || t.contains("search")
        || t.contains("find ")
    {
        return "react";
    }
    if t.contains("write")
        || t.contains("draft")
        || t.contains("report")
        || t.contains("polish")
        || t.contains("review")
    {
        return "reflect";
    }
    if t.contains("design")
        || t.contains("compare")
        || t.contains("options")
        || t.contains("approach")
        || t.contains("brainstorm")
    {
        return "explore-branch";
    }
    "plan-execute"
}

pub async fn propose_loop(
    State(state): State<AppState>,
    Json(req): Json<ProposeLoopRequest>,
) -> AppResult<Json<ProposeLoopResponse>> {
    let cluster = state.cluster().ok_or(AppError::ClusterUnavailable)?;
    let intent = req.intent.trim();
    if intent.is_empty() {
        return Err(AppError::BadRequest("intent is required".into()));
    }
    let surface = if req.surface == "team" {
        "team"
    } else {
        "mission"
    };
    let heuristic = heuristic_pattern(intent, surface);

    let options = build_options(cluster).await?;
    // Don't fabricate a specific model when none is configured — an empty model
    // makes the orchestrator call fail cleanly and fall back to the heuristic
    // below, rather than pretending a named model exists on this cluster.
    let default_model = options
        .models
        .iter()
        .find(|m| m.is_default)
        .or_else(|| options.models.first())
        .map(|m| m.deployment.clone())
        .unwrap_or_default();

    let system = format!(
        "You are a loop-engineering orchestrator. Given a user's intent, pick the ONE feedback-loop \
         pattern that best fits and draft the loop. Patterns: react (reason+act with tools), reflect \
         (draft, self-critique, revise), plan-execute (plan then do), eval-iterate (define acceptance \
         checks first, loop until they pass), explore-branch (generate candidates, prune), \
         standing-watch (periodic observe->detect change->act, for standing {surface} work). \
         Respond with ONLY a JSON object: {{\"pattern\": one of [{}], \"goal\": string, \
         \"criteria\": string with one success criterion per line, \"rationale\": one short sentence}}.",
        LOOP_PATTERN_IDS.join(", ")
    );
    let user = format!(
        "Surface: {surface}\nIntent:\n{intent}\n\nA reasonable default pattern is '{heuristic}', but \
         choose the best fit. Respond with ONLY the JSON object."
    );

    // Ask the orchestrator; parse its JSON. Any failure → honest heuristic.
    match orchestrator_complete(
        cluster,
        &system,
        &user,
        &default_model,
        LOOP_COMPOSE_MAX_TOKENS,
    )
    .await
    {
        Ok((raw, _src)) => {
            if let Some(v) = extract_json_object(&raw) {
                let pattern = v
                    .get("pattern")
                    .and_then(|p| p.as_str())
                    .filter(|p| LOOP_PATTERN_IDS.contains(p))
                    .unwrap_or(heuristic)
                    .to_string();
                let goal = v
                    .get("goal")
                    .and_then(|g| g.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| intent.to_string());
                let criteria = v
                    .get("criteria")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let rationale = v
                    .get("rationale")
                    .and_then(|r| r.as_str())
                    .unwrap_or("Best fit for this intent.")
                    .trim()
                    .to_string();
                return Ok(Json(ProposeLoopResponse {
                    pattern,
                    goal,
                    criteria,
                    rationale,
                    source: "orchestrator".into(),
                }));
            }
            // Unparseable model output → heuristic.
        }
        Err(_) => { /* orchestrator unreachable → heuristic */ }
    }

    Ok(Json(ProposeLoopResponse {
        pattern: heuristic.to_string(),
        goal: intent.to_string(),
        criteria: String::new(),
        rationale: "Chosen from your intent's keywords (orchestrator unavailable).".into(),
        source: "heuristic".into(),
    }))
}

/// Find and parse the first top-level JSON object in a model response (it may be
/// fenced or prefixed with prose). Returns `None` when there's no parseable object.
fn extract_json_object(raw: &str) -> Option<serde_json::Value> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str(&raw[start..=end]).ok()
}

/// Complete the orchestrator prompt, returning `(raw_model_output, source)`.
///
/// Two reachable paths, in priority order:
///   1. **Ops override** — an explicit `BRIDGE_ORCHESTRATOR_{ENDPOINT,TOKEN,MODEL}`
///      triple (a dedicated composer endpoint the operator configured).
///   2. **Native** — route through a Running sandbox's inference router via the
///      `pods/proxy` subresource. The router injects the provider's auth +
///      integration headers (Copilot/Foundry) and enforces governance, so this
///      works on workload-identity clusters with NO static token in the Bridge —
///      reusing exactly the secure path agents use.
async fn orchestrator_complete(
    cluster: &crate::kars::cluster::Cluster,
    system: &str,
    user: &str,
    default_model: &str,
    max_tokens: u32,
) -> anyhow::Result<(String, String)> {
    // 1. Ops override — direct endpoint/token/model.
    if let (Ok(endpoint), Ok(token), Ok(model)) = (
        std::env::var("BRIDGE_ORCHESTRATOR_ENDPOINT"),
        std::env::var("BRIDGE_ORCHESTRATOR_TOKEN"),
        std::env::var("BRIDGE_ORCHESTRATOR_MODEL"),
    ) && !endpoint.trim().is_empty()
        && !token.trim().is_empty()
        && !model.trim().is_empty()
    {
        let raw = call_llm(&endpoint, &token, &model, system, user, max_tokens).await?;
        return Ok((raw, model));
    }

    // 2. Native — through a running sandbox's secure inference router. Try each
    //    stable candidate in turn so a sandbox with stale provider auth or a
    //    warming router is skipped rather than failing the whole compose.
    //    Claude models use the native Anthropic `/v1/messages` path (the
    //    OpenAI-compat path returns empty content for Claude).
    let candidates = cluster.running_sandbox_candidates().await;
    if candidates.is_empty() {
        anyhow::bail!(
            "orchestrator has no inference path yet — the standing `bridge-orchestrator` sandbox is still starting (retry shortly), or set BRIDGE_ORCHESTRATOR_{{ENDPOINT,TOKEN,MODEL}} to route directly at Azure AI Foundry / Azure OpenAI (scales better for many teams)"
        );
    }
    let is_claude = default_model.to_ascii_lowercase().contains("claude");
    let mut last_err = String::from("no candidate router returned content");
    for (ns, pod) in candidates.iter().take(4) {
        match orchestrator_via_router(
            cluster,
            ns,
            pod,
            default_model,
            OrchestratorPrompt { system, user },
            is_claude,
            max_tokens,
        )
        .await
        {
            Ok(content) if !content.trim().is_empty() => {
                return Ok((content, format!("{default_model} (cluster router)")));
            }
            Ok(_) => last_err = "router returned empty content".into(),
            Err(e) => last_err = e.to_string(),
        }
    }
    anyhow::bail!("{last_err}")
}

struct OrchestratorPrompt<'a> {
    system: &'a str,
    user: &'a str,
}

/// Single orchestrator completion against one sandbox router (Anthropic
/// `/v1/messages` for Claude, OpenAI `/chat/completions` otherwise).
async fn orchestrator_via_router(
    cluster: &crate::kars::cluster::Cluster,
    ns: &str,
    pod: &str,
    model: &str,
    prompt: OrchestratorPrompt<'_>,
    is_claude: bool,
    max_tokens: u32,
) -> anyhow::Result<String> {
    let OrchestratorPrompt { system, user } = prompt;
    if is_claude {
        let body = serde_json::json!({
            "model": model,
            "system": system,
            "messages": [{ "role": "user", "content": user }],
            "max_tokens": max_tokens,
        });
        let text = cluster.router_messages(ns, pod, &body).await?;
        let parsed: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("router returned non-JSON: {e}"))?;
        let content: String = parsed
            .get("content")
            .and_then(|c| c.as_array())
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        return Ok(content);
    }

    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "max_tokens": max_tokens,
    });
    let text = cluster.router_chat(ns, pod, &body).await?;
    let parsed: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("router returned non-JSON: {e}"))?;
    Ok(parsed
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or_default()
        .to_string())
}

/// Build the system prompt enumerating the real building blocks + the strict
/// JSON contract. The model is told it may ONLY use these exact identifiers,
/// and is given the learned efficiency frontier so its model choice is grounded
/// in what actually performs on this cluster — not a blind pick.
fn build_system_prompt(
    o: &crate::routes::options::Options,
    eff: &crate::routes::efficiency::EfficiencyDto,
    qualification_constraints: &str,
    resource_qualification_constraints: &str,
) -> String {
    let models = o
        .models
        .iter()
        .map(|m| {
            format!(
                "  - deployment=\"{}\" provider=\"{}\"{}",
                m.deployment,
                m.provider,
                if m.is_default { " (default)" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let runtimes = o
        .runtimes
        .iter()
        .filter(|r| r.wired && r.kind != "BYO")
        .map(|r| format!("  - \"{}\"", r.kind))
        .collect::<Vec<_>>()
        .join("\n");
    let isolation = o
        .isolation
        .iter()
        .map(|i| format!("  - \"{}\" — {}", i.value, i.note))
        .collect::<Vec<_>>()
        .join("\n");
    let policies = if o.tool_policies.is_empty() {
        "  (none)".to_string()
    } else {
        o.tool_policies
            .iter()
            .map(|p| {
                format!(
                    "  - \"{}\"{}",
                    p.name,
                    p.summary
                        .as_deref()
                        .map(|s| format!(" — {s}"))
                        .unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mcp = if o.mcp_servers.is_empty() {
        "  (none)".to_string()
    } else {
        o.mcp_servers
            .iter()
            .map(|m| {
                format!(
                    "  - \"{}\"{}{}{}{}{}",
                    m.name,
                    m.summary
                        .as_deref()
                        .map(|s| format!(" — {s}"))
                        .unwrap_or_default(),
                    m.mode
                        .as_deref()
                        .map(|mode| format!(" · mode={mode}"))
                        .unwrap_or_default(),
                    if m.discovered_tools.is_empty() {
                        String::new()
                    } else {
                        format!(" · tools=[{}]", m.discovered_tools.join(", "))
                    },
                    m.tool_schema_digest
                        .as_deref()
                        .map(|digest| format!(" · schema_digest={digest}"))
                        .unwrap_or_else(|| " · schema_digest=missing".into()),
                    m.readiness
                        .as_deref()
                        .map(|readiness| format!(" · readiness={readiness}"))
                        .unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let memories = if o.memories.is_empty() {
        "  (none)".to_string()
    } else {
        o.memories
            .iter()
            .map(|m| {
                format!(
                    "  - \"{}\"{}{}{}{}",
                    m.name,
                    m.summary
                        .as_deref()
                        .map(|summary| format!(" — {summary}"))
                        .unwrap_or_default(),
                    m.backend
                        .as_deref()
                        .map(|backend| format!(" · backend={backend}"))
                        .unwrap_or_else(|| " · backend=missing".into()),
                    m.compiled_digest
                        .as_deref()
                        .map(|digest| format!(" · compiled_digest={digest}"))
                        .unwrap_or_else(|| " · compiled_digest=missing".into()),
                    m.readiness
                        .as_deref()
                        .map(|readiness| format!(" · readiness={readiness}"))
                        .unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let skills = if o.skills.is_empty() {
        "  (none)".to_string()
    } else {
        o.skills
            .iter()
            .map(|s| {
                format!(
                    "  - \"{}\"{}{}{}{}{}",
                    s.name,
                    s.summary
                        .as_deref()
                        .map(|v| format!(" — {v}"))
                        .unwrap_or_default(),
                    s.version
                        .as_deref()
                        .map(|version| format!(" · version={version}"))
                        .unwrap_or_default(),
                    s.version_digest
                        .as_deref()
                        .map(|digest| format!(" · version_digest={digest}"))
                        .unwrap_or_else(|| " · version_digest=missing".into()),
                    s.recipe
                        .as_deref()
                        .map(|recipe| {
                            format!(" · recipe={}", recipe.chars().take(180).collect::<String>())
                        })
                        .unwrap_or_default(),
                    s.readiness
                        .as_deref()
                        .map(|readiness| format!(" · readiness={readiness}"))
                        .unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    // The learned efficiency frontier — grounds the model choice in real
    // outcomes. Honest: when no runs have completed yet, say so rather than
    // inventing a recommendation.
    let efficiency = if eff.routes.is_empty() {
        "  (no completed runs yet — choose the default model unless the objective clearly warrants another)".to_string()
    } else {
        let mut lines = eff
            .routes
            .iter()
            .take(6)
            .map(|r| {
                // Structured, honest per-route signal. Absent metrics (pass^k
                // with no repeats, USD with no price table) are omitted rather
                // than faked, so the model never reasons over invented numbers.
                let reliability = match (r.reliability_rate, r.reliability_k) {
                    (Some(rate), Some(k)) => {
                        format!(", pass^{k} reliability {:.0}% (n={})", rate * 100.0, r.reliability_samples)
                    }
                    _ => String::new(),
                };
                let latency = if r.avg_wall_ms > 0 {
                    format!(", ~{:.0}s wall (p95 {:.0}s)", r.avg_wall_ms as f64 / 1000.0, r.p95_wall_ms as f64 / 1000.0)
                } else {
                    String::new()
                };
                let toolfail = if r.avg_tool_calls > 0.0 {
                    format!(", {:.0}% tool-fail", r.tool_fail_rate * 100.0)
                } else {
                    String::new()
                };
                let usd = match r.usd_per_outcome {
                    Some(u) => format!(", ${:.3}/outcome", u),
                    None => String::new(),
                };
                let fault = if r.top_fault.is_empty() {
                    String::new()
                } else {
                    format!(", top miss: {}", r.top_fault)
                };
                format!(
                    "  - route \"{}\": {:.0}% accepted, {:.0}% delivered, {} tokens/outcome{usd}{reliability}{latency}{toolfail}{fault} over {} run(s){}",
                    r.route,
                    r.acceptance_rate * 100.0,
                    r.success_rate * 100.0,
                    r.tokens_per_outcome,
                    r.runs,
                    if !eff.recommended_low_confidence
                        && eff.recommended.as_deref() == Some(r.route.as_str())
                    {
                        "  ← recommended"
                    } else if eff.recommended_low_confidence
                        && eff.recommended.as_deref() == Some(r.route.as_str())
                    {
                        "  ← best observed, insufficient evidence"
                    } else {
                        ""
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !eff.recommended_low_confidence
            && let Some(rec) = &eff.recommended
        {
            lines.push_str(&format!(
                "\n  Prefer the recommended route's model (route contains its deployment: \"{rec}\") unless the objective clearly needs a stronger or cheaper model."
            ));
        } else if eff.recommended_low_confidence {
            lines.push_str(
                "\n  The retained route history is too sparse for automatic model selection. Use the cluster default unless the objective itself clearly requires a stronger or more specialized model.",
            );
        }
        lines
    };

    format!(
        r#"You are the kars launch-package orchestrator. You turn a user's plain-language objective into a single, well-governed launch package for a sandboxed AI agent on the kars runtime. You propose; a human reviews and approves before anything runs.

You MUST only use the building blocks listed below — never invent a model, tool policy, MCP server, isolation level, or memory store that is not listed.

AVAILABLE MODELS (pick exactly one by its deployment string):
{models}

EFFICIENCY FRONTIER (learned from completed runs on THIS cluster — the honest signal is human ACCEPTANCE, not emitted tokens):
{efficiency}

QUALIFIED EXECUTION RECORDS (the complete proposed package MUST fit one record; records do not compose):
{qualification_constraints}

RESOURCE QUALIFICATION RECORDS (selected MCP servers, memory bindings, and skills MUST match one current-digest record on the chosen route; generic route records do not count):
{resource_qualification_constraints}

MODEL ROUTING: the model running this composer is not automatically the model that should execute the mission. For routine, bounded, low-risk work, prefer the cluster default or a proven efficient route. Reserve the strongest model for objectives with substantial ambiguity, synthesis, security impact, long context, or difficult tool orchestration. Sparse history with few or zero accepted outcomes is not a recommendation.

HARNESSES (pick exactly one):
{runtimes}

ISOLATION LEVELS (pick exactly one):
{isolation}

TOOL POLICIES (optional; pick one name or null):
{policies}

MCP SERVERS (optional; pick zero or more names; if you pick any, you MUST also set a tool_policy):
{mcp}
Foundry-native web search, file search, memory, and code execution are Kars plugin tools and do not require MCP. If the customer explicitly requests an installed MCP server, select it and declare `mcp`; the complete capability combination must match one atomic qualification record.

APPROVED SKILLS (optional; pick zero or more names):
{skills}

SHARED MEMORY STORES (optional; pick one name or null):
{memories}

AUTONOMY TIERS (pick the lowest tier that fits the objective):
  1 = Manual (proposes every step, acts on nothing)
  2 = Shared (acts only on low-risk steps)
  3 = Conditional (acts, but pauses before anything costly/external/irreversible)
  4 = Supervised (autonomous with periodic checkpoints)
  5 = Full (fully autonomous within budget)
Default to tier 3 unless the objective clearly warrants more or less.

EGRESS: list the external network hosts the agent legitimately needs (e.g. an API host), as objects {{"host": "...", "port": 443}}. Prefer an empty list — the model path is always allowed; only add hosts the task truly requires.

INSTRUCTIONS: write a concise, specific system prompt (2–5 sentences) framing the agent's role and standards for THIS objective.

EXECUTION PLAN: when the objective benefits from decomposition, propose a workload-neutral typed execution plan. Choose arbitrary role names from the objective — never use a fixed role template. Each role has dependency-aware phases. Each phase declares only the generic capabilities it needs: filesystem-read, filesystem-write, shell, network, web-search, mcp, memory. `min_tool_calls` is the minimum successful evidence-producing calls required; set it to at least 1 whenever the phase outcome depends on tools or external evidence. `max_tool_calls` is the explicit upper bound. Set `fresh_context=true` when a phase should consume only prior handbacks instead of the full earlier transcript. Use null for a small single-agent objective.

LAUNCHABILITY: every required capability, the runtime/model route, and max_parallel MUST fit one qualified execution record above. Never merge capabilities from separate records. If you select an MCP server, memory binding, or approved skill, state in the rationale which current-digest resource qualification record makes it launchable. Generic route records do not prove a specific server, backend, or skill version. If the requested output format requires an unqualified capability, propose a supported alternative (for example a Markdown report with inline Mermaid diagrams instead of generated binary images) and explain that choice in the rationale. When you emit Mermaid flowcharts, quote every label that contains parser-sensitive punctuation such as :, (), [], {{}}, or /.

RESEARCH EVIDENCE: for current-events, incident, or authoritative-source research, declare `web-search` on the source-discovery phase and `network` on the exact-URL fetch phase (or declare both on one combined phase). The first fetch-capable phase must discover exact source URLs with an available search tool (`foundry_web_search` or `web_search`) before fetching pages. Never invent article paths. A timeout, non-success response, blocked page, or search snippet is not evidence for a factual claim. Later phases and synthesis may cite only URLs and facts retained from successful source-discovery/fetch tool results; if authoritative evidence is unavailable, report the gap instead of reconstructing unsupported details.

BUDGET: optionally propose a token budget (integer) appropriate to the scope, or null for no cap.

Respond with ONLY a JSON object (no prose, no code fences) of exactly this shape:
{{
  "tier": <int 1-5>,
  "model": {{"provider": "<provider>", "deployment": "<deployment>"}},
  "runtime": "<harness>",
  "instructions": "<system prompt>",
  "tool_policy": "<name or null>",
  "mcp_servers": ["<name>", ...],
  "skills": ["<name>", ...],
  "egress": [{{"host": "<host>", "port": <int or null>}}],
  "isolation": "<level>",
  "memory": "<name or null>",
  "budget_tokens": <int or null>,
  "execution_plan": {{
    "schema": "kars.execution-plan/v1",
    "roles": [{{
      "name": "<short-kebab-role>",
      "objective": "<narrow assignment>",
      "depends_on": ["<earlier-role>", ...],
      "budget_tokens": <int or null>,
      "phases": [{{
        "name": "<short-kebab-phase>",
        "objective": "<phase outcome>",
        "capabilities": ["<filesystem-read|filesystem-write|shell|network|web-search|mcp|memory>", ...],
        "min_tool_calls": <int 0-32, <= max_tool_calls>,
        "max_tool_calls": <int 0-32>,
        "fresh_context": <bool>
      }}]
    }}],
    "max_parallel": <int 1-8>,
    "synthesis": {{
      "objective": "<how the principal should reconcile handbacks>",
      "capabilities": [],
      "max_tool_calls": 0
    }},
    "deliverables": [{{"name":"<safe filename>","media_type":"<optional MIME>"}}]
  }} | null,
  "rationale": "<1-3 sentences explaining the key choices for the reviewer>"
}}"#
    )
}

/// Call the orchestrator LLM (OpenAI-compatible chat/completions) and return
/// the assistant's text content.
async fn call_llm(
    endpoint: &str,
    token: &str,
    model: &str,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> anyhow::Result<String> {
    let url = format!("{}/chat/completions", endpoint.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "max_tokens": max_tokens,
    });
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        .build()?;
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!(
            "HTTP {status}: {}",
            text.chars().take(200).collect::<String>()
        );
    }
    let parsed: serde_json::Value = serde_json::from_str(&text)?;
    let content = parsed
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("no completion content"))?;
    Ok(content)
}

/// Parse the model's JSON (tolerating code fences / surrounding prose) and
/// Whether a harness runs a PRODUCTIVE one-shot / cadence autonomous session —
/// i.e. it consumes a delivered objective (over the mesh) and returns a real
/// deliverable, with no human chat channel required. Only these may back an
/// autonomous mission or a standing team member.
///
/// - `OpenClaw` — the reference autonomous harness (always-on agent loop).
/// - `Hermes` — verified autonomous: its mesh worker executes a delivered
///   `task_request` in-process and replies (the "idle daemon" boot line is a
///   red herring — the gateway still loads the plugin + worker). Confirmed E2E.
/// - `BYO` — the operator's own image, contractually responsible for consuming
///   the objective + delivering; we take it at its word rather than downgrade it.
///
/// Everything else in the catalogue today (Anthropic / OpenAIAgents / MAF /
/// LangGraph / PydanticAi) is a **bootstrap-only** adapter: it pins a provider
/// URL + OTel and exits, with no task-execution loop, so it delivers NOTHING
/// autonomously. Routing autonomous work to one is a silent no-op — so we
/// correct it to OpenClaw (attested), exactly like the mission path.
pub(crate) fn is_autonomous_harness(kind: &str) -> bool {
    kind.eq_ignore_ascii_case("OpenClaw")
        || kind.eq_ignore_ascii_case("Hermes")
        || kind.eq_ignore_ascii_case("BYO")
}

/// Inverse of [`is_autonomous_harness`]: a harness that cannot run a productive
/// autonomous session (a bootstrap-only SDK/graph adapter today). Autonomous
/// missions and team members routed to one must be corrected/refused.
pub(crate) fn is_non_autonomous_harness(kind: &str) -> bool {
    !kind.trim().is_empty() && !is_autonomous_harness(kind)
}

/// validate every field against the real options — the server is the authority,
/// not the model. Anything invalid is dropped or normalized to a safe default.
fn parse_and_validate(
    raw: &str,
    o: &crate::routes::options::Options,
    eff: &crate::routes::efficiency::EfficiencyDto,
    intent: &str,
) -> (ComposeProposal, Option<String>) {
    let json = extract_json(raw).unwrap_or_else(|| serde_json::json!({}));

    // Tier: clamp to 1..=5, default 3.
    let tier = json
        .get("tier")
        .and_then(|v| v.as_i64())
        .map(|t| t.clamp(1, 5) as i32)
        .unwrap_or(3);

    // ── Model selection (efficiency-driven) ─────────────────────────────────
    // Priority: (1) the orchestrator's explicit, valid choice — objective-aware;
    // (2) the learned efficiency frontier's recommended route — grounded in real
    // accepted outcomes; (3) the cluster default; (4) the first catalogue model.
    // `model_basis` records which of these fired so the reviewer sees WHY.
    let orchestrator_pick = json.get("model").and_then(|m| {
        let provider = m.get("provider").and_then(|p| p.as_str())?;
        let dep = m.get("deployment").and_then(|d| d.as_str())?;
        o.models
            .iter()
            .find(|mo| mo.provider == provider && mo.deployment == dep)
            .map(|mo| ComposeModel {
                provider: mo.provider.clone(),
                deployment: mo.deployment.clone(),
            })
    });

    let recommended_model = eff
        .recommended
        .as_ref()
        .filter(|_| {
            recommendation_is_actionable(eff.recommended.as_deref(), eff.recommended_low_confidence)
        })
        .and_then(|route| {
            o.models
                .iter()
                .find(|mo| mo.deployment == *route)
                .map(|mo| ComposeModel {
                    provider: mo.provider.clone(),
                    deployment: mo.deployment.clone(),
                })
        });

    let (model, mut model_basis, model_from_reco) = if let Some(m) = orchestrator_pick {
        let is_reco = recommendation_is_actionable(
            eff.recommended.as_deref(),
            eff.recommended_low_confidence,
        ) && eff.recommended.as_ref().is_some_and(|r| *r == m.deployment);
        let basis = if is_reco {
            efficiency_basis(eff, &m.deployment)
        } else {
            "Chosen by the orchestrator for this objective.".to_string()
        };
        (Some(m), Some(basis), is_reco)
    } else if let Some(m) = recommended_model {
        let basis = efficiency_basis(eff, &m.deployment);
        (Some(m), Some(basis), true)
    } else {
        let m = o
            .models
            .iter()
            .find(|m| m.is_default)
            .or_else(|| o.models.first())
            .map(|m| ComposeModel {
                provider: m.provider.clone(),
                deployment: m.deployment.clone(),
            });
        let basis = m.as_ref().map(|_| {
            if eff.total_runs == 0 {
                "Cluster default — no completed runs yet to learn a better route.".to_string()
            } else if eff.recommended.is_some() {
                // There IS a learned recommendation, but it doesn't map to a live
                // model — be honest rather than implying the default was "chosen".
                "Cluster default — the recommended route is no longer in the catalogue.".to_string()
            } else {
                "Cluster default — the objective didn't clearly warrant another route.".to_string()
            }
        });
        (m, basis, false)
    };

    // Runtime: the orchestrator's valid choice wins; else, when the model came
    // from the efficiency frontier, adopt the harness that ACTUALLY won on that
    // route (so we propose the whole winning route, not the model on a default
    // harness); else OpenClaw. Any adopted harness must be a wired runtime.
    let mut runtime = json
        .get("runtime")
        .and_then(|v| v.as_str())
        .filter(|r| o.runtimes.iter().any(|ro| ro.wired && ro.kind == *r))
        .map(str::to_string)
        .or_else(|| {
            if model_from_reco {
                eff.recommended_harness
                    .as_ref()
                    .filter(|h| o.runtimes.iter().any(|ro| ro.wired && ro.kind == **h))
                    .cloned()
            } else {
                None
            }
        })
        .unwrap_or_else(|| "OpenClaw".to_string());

    // Honesty guard (B1): the model can come from the recommended route while
    // the orchestrator proposes a DIFFERENT harness. In that case the basis must
    // NOT imply we adopted the whole recommended route — the reviewer was seeing
    // "Best learned route … on OpenClaw" next to a package that actually ran on
    // Hermes. Rewrite the basis to name the real divergence.
    if model_from_reco
        && let Some(rec_h) = eff.recommended_harness.as_deref()
        && !rec_h.is_empty()
        && rec_h != runtime
    {
        let dep = model
            .as_ref()
            .map(|m| m.deployment.clone())
            .unwrap_or_else(|| "the recommended model".to_string());
        model_basis = Some(format!(
            "Recommended model ({dep}), proposed on the {runtime} harness — \
                     note the learned best route ran on {rec_h}, so this is not the \
                     full recommended route."
        ));
    }

    // ── Hard capability match (0.4) ─────────────────────────────────────────
    // A one-shot MISSION requires an autonomous harness — one that consumes a
    // delivered objective and returns a deliverable. A bootstrap-only adapter
    // (Anthropic/OpenAIAgents/MAF/LangGraph/PydanticAi) has no task-execution
    // loop and delivers nothing autonomously, so routing a mission to one is a
    // silent no-op. This is a capability mismatch, not a preference, so we BLOCK
    // it rather than soft-warn: it's corrected to OpenClaw (the autonomous
    // default) and the decision is recorded in the rationale + stamped on the
    // task at launch (kars.azure.com/harness-corrected). (Hermes and BYO are
    // autonomous and pass through unchanged.)
    let harness_correction: Option<String> = if is_non_autonomous_harness(&runtime) {
        let note = format!(
            "Capability match: {runtime} is a bootstrap-only adapter with no autonomous \
             task-execution loop, so it cannot run a one-shot mission — routed to OpenClaw \
             (autonomous harness).",
        );
        runtime = "OpenClaw".to_string();
        Some(note)
    } else {
        None
    };

    // Isolation: must be a real level; else standard.
    let isolation = json
        .get("isolation")
        .and_then(|v| v.as_str())
        .filter(|i| o.isolation.iter().any(|io| io.value == *i))
        .unwrap_or("standard")
        .to_string();

    // Tool policy: must be a real policy name. Every envelope MUST carry a
    // governance policy — the agent runtime always initializes its AGT engine
    // and fails closed on an empty policy set, so an envelope with no tool
    // policy yields a sandbox that hangs (no tool/inference/mesh is allowed).
    // When the orchestrator doesn't name one, fall back to the cluster default
    // governance policy (`kars-default`, which allows inference/tool/mesh/spawn
    // and denies dangerous shell) so the sandbox is governed AND functional.
    let requested_tool_policy = json
        .get("tool_policy")
        .and_then(|v| v.as_str())
        .filter(|p| !p.is_empty() && o.tool_policies.iter().any(|tp| tp.name == *p))
        .map(str::to_string)
        .or_else(|| default_tool_policy(o));
    let (tool_policy, policy_correction) = if requested_tool_policy.as_deref()
        == Some("kars-team-member")
    {
        (
                default_tool_policy(o),
                Some(
                    "Capability match: kars-team-member is reserved for declared standing-team specialists; routed this standalone mission to kars-default."
                        .to_string(),
                ),
            )
    } else {
        (requested_tool_policy, None)
    };

    // MCP servers: subset of real servers.
    let mut mcp_servers: Vec<String> = json
        .get("mcp_servers")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .filter(|name| o.mcp_servers.iter().any(|m| m.name == *name))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    // Least privilege: a model sometimes lists the same server more than once —
    // dedupe (order-preserving) so the package never carries a redundant MCP
    // grant (the audit saw the same Playwright server selected twice).
    {
        let mut seen = std::collections::HashSet::new();
        mcp_servers.retain(|s| seen.insert(s.clone()));
    }
    // Governance invariant: MCP access requires a bounding tool policy. If the
    // model asked for MCP without one, drop the MCP servers rather than emit an
    // un-admittable package (the reviewer can re-add with a policy).
    if !mcp_servers.is_empty() && tool_policy.is_none() {
        mcp_servers.clear();
    }

    let requested_skills: Vec<String> = json
        .get("skills")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .filter(|name| o.skills.iter().any(|s| s.name == *name))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let (skills, skill_correction) = if runtime == "OpenClaw" {
        (requested_skills, None)
    } else if requested_skills.is_empty() {
        (Vec::new(), None)
    } else {
        (
            Vec::new(),
            Some(format!(
                "Capability match: {runtime} does not support controller-mounted file skills; omitted them instead of claiming they would be installed."
            )),
        )
    };

    // Memory: must be a real store; else None.
    let memory = json
        .get("memory")
        .and_then(|v| v.as_str())
        .filter(|m| !m.is_empty() && o.memories.iter().any(|mo| mo.name == *m))
        .map(str::to_string);

    // Egress: sanitized host list.
    let egress = json
        .get("egress")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let host = e.get("host").and_then(|h| h.as_str())?.trim().to_string();
                    if host.is_empty() {
                        return None;
                    }
                    let port = e
                        .get("port")
                        .and_then(|p| p.as_u64())
                        .and_then(|p| u16::try_from(p).ok());
                    Some(ComposeEgress { host, port })
                })
                .take(20)
                .collect()
        })
        .unwrap_or_default();
    let egress = complete_egress_recommendation(egress, intent, &mcp_servers);

    let instructions = json
        .get("instructions")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    let execution_plan = parse_execution_plan(&json);
    let delegation = execution_plan
        .as_ref()
        .map(delegation_from_execution_plan)
        .unwrap_or_else(single_agent_delegation);
    let proposed_budget = json
        .get("budget_tokens")
        .and_then(|v| v.as_i64())
        .filter(|t| *t > 0)
        .filter(|tokens| *tokens > 0);
    let budget_tokens = proposed_budget;

    let rationale = json
        .get("rationale")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    // Surface the capability correction to the reviewer alongside the rationale so
    // the harness swap is never silent.
    let corrections = [
        harness_correction.as_deref(),
        policy_correction.as_deref(),
        skill_correction.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ");
    let rationale = match (rationale, corrections.is_empty()) {
        (Some(r), false) => Some(format!("{r} {corrections}")),
        (None, false) => Some(corrections),
        (r, true) => r,
    };

    (
        ComposeProposal {
            tier,
            model,
            model_fallbacks: Vec::new(),
            model_basis,
            runtime,
            instructions,
            tool_policy,
            mcp_servers,
            skills,
            egress,
            isolation,
            memory,
            budget_tokens,
            execution_plan,
            delegation,
        },
        rationale,
    )
}

fn valid_delegation_role(value: &serde_json::Value) -> Option<ComposeDelegationRole> {
    let name = value.get("name")?.as_str()?.trim().to_ascii_lowercase();
    let objective = value.get("objective")?.as_str()?.trim().to_string();
    if name.is_empty()
        || name.len() > 48
        || objective.len() < 20
        || objective.len() > 600
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || name.starts_with('-')
        || name.ends_with('-')
    {
        return None;
    }
    Some(ComposeDelegationRole { name, objective })
}

fn single_agent_delegation() -> ComposeDelegation {
    ComposeDelegation {
        mode: "single-agent".into(),
        roles: Vec::new(),
        max_parallel: 1,
    }
}

fn delegation_from_execution_plan(
    plan: &crate::routes::tasks::ExecutionPlanDto,
) -> ComposeDelegation {
    ComposeDelegation {
        mode: "principal-specialists".into(),
        roles: plan
            .roles
            .iter()
            .map(|role| ComposeDelegationRole {
                name: role.name.clone(),
                objective: role.objective.clone(),
            })
            .collect(),
        max_parallel: plan.max_parallel,
    }
}

const EXECUTION_CAPABILITIES: &[&str] = &[
    "filesystem-read",
    "filesystem-write",
    "shell",
    "network",
    "web-search",
    "mcp",
    "memory",
];

const EXECUTION_ROLE_PHASE_BASE_WEIGHT: i64 = 4;

fn valid_plan_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 48
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

fn valid_capabilities(capabilities: &[String]) -> bool {
    let mut seen = std::collections::HashSet::new();
    capabilities.iter().all(|capability| {
        EXECUTION_CAPABILITIES.contains(&capability.as_str()) && seen.insert(capability)
    })
}

fn role_budget_weight(role: &crate::routes::tasks::ExecutionRoleDto) -> i64 {
    role.phases
        .iter()
        .map(|phase| EXECUTION_ROLE_PHASE_BASE_WEIGHT + i64::from(phase.max_tool_calls))
        .sum::<i64>()
        .max(1)
}

fn weighted_role_budget_floors(
    total_tokens: i64,
    plan: &crate::routes::tasks::ExecutionPlanDto,
) -> Vec<i64> {
    let weights = plan
        .roles
        .iter()
        .map(role_budget_weight)
        .collect::<Vec<_>>();
    let total_weight = weights
        .iter()
        .map(|weight| i128::from(*weight))
        .sum::<i128>();
    let total_tokens_i128 = i128::from(total_tokens);
    let mut floors = weights
        .iter()
        .map(|weight| ((total_tokens_i128 * i128::from(*weight)) / total_weight) as i64)
        .collect::<Vec<_>>();
    let assigned = floors.iter().sum::<i64>();
    let mut remainders = weights
        .iter()
        .enumerate()
        .map(|(index, weight)| {
            (
                (total_tokens_i128 * i128::from(*weight)) % total_weight,
                index,
            )
        })
        .collect::<Vec<_>>();
    remainders.sort_by(
        |(left_remainder, left_index), (right_remainder, right_index)| {
            right_remainder
                .cmp(left_remainder)
                .then(left_index.cmp(right_index))
        },
    );
    for (_, index) in remainders
        .into_iter()
        .take((total_tokens - assigned) as usize)
    {
        floors[index] += 1;
    }
    floors
}

fn apply_weighted_role_budget_floors(
    plan: &mut crate::routes::tasks::ExecutionPlanDto,
    total_tokens: i64,
) -> (bool, i64) {
    let mut changed = false;
    let floors = weighted_role_budget_floors(total_tokens, plan);
    for (role, floor) in plan.roles.iter_mut().zip(floors) {
        let floor = floor.max(1);
        if role.budget_tokens.is_none_or(|current| current < floor) {
            role.budget_tokens = Some(floor);
            changed = true;
        }
    }
    let total = plan
        .roles
        .iter()
        .filter_map(|role| role.budget_tokens)
        .sum();
    (changed, total)
}

fn valid_deliverable_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && !name.contains('/')
        && !name.contains('\\')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn execution_plan_is_acyclic(plan: &crate::routes::tasks::ExecutionPlanDto) -> bool {
    let dependencies = plan
        .roles
        .iter()
        .map(|role| (role.name.as_str(), role.depends_on.as_slice()))
        .collect::<std::collections::HashMap<_, _>>();
    fn visit<'a>(
        role: &'a str,
        dependencies: &std::collections::HashMap<&'a str, &'a [String]>,
        visiting: &mut std::collections::HashSet<&'a str>,
        visited: &mut std::collections::HashSet<&'a str>,
    ) -> bool {
        if visited.contains(role) {
            return true;
        }
        if !visiting.insert(role) {
            return false;
        }
        for dependency in dependencies.get(role).copied().unwrap_or_default() {
            if !visit(dependency, dependencies, visiting, visited) {
                return false;
            }
        }
        visiting.remove(role);
        visited.insert(role);
        true
    }
    let mut visiting = std::collections::HashSet::new();
    let mut visited = std::collections::HashSet::new();
    plan.roles
        .iter()
        .all(|role| visit(&role.name, &dependencies, &mut visiting, &mut visited))
}

pub(crate) fn validate_execution_plan(
    plan: &crate::routes::tasks::ExecutionPlanDto,
) -> Result<(), String> {
    if plan.schema != "kars.execution-plan/v1" {
        return Err("execution plan schema must be kars.execution-plan/v1".into());
    }
    if plan.roles.is_empty() || plan.roles.len() > 8 {
        return Err("execution plan requires 1-8 roles".into());
    }
    if plan.max_parallel < 1 || plan.max_parallel > plan.roles.len() as i32 {
        return Err("execution plan max_parallel must be within the role count".into());
    }
    let role_names = plan
        .roles
        .iter()
        .map(|role| role.name.as_str())
        .collect::<std::collections::HashSet<_>>();
    if role_names.len() != plan.roles.len() || role_names.iter().any(|name| !valid_plan_name(name))
    {
        return Err("execution plan role names must be unique DNS-safe labels".into());
    }
    for role in &plan.roles {
        if !(20..=1200).contains(&role.objective.len()) {
            return Err(format!("role {} has an invalid objective", role.name));
        }
        if role.phases.is_empty() || role.phases.len() > 8 {
            return Err(format!("role {} requires 1-8 phases", role.name));
        }
        if role.budget_tokens.is_some_and(|budget| budget <= 0) {
            return Err(format!("role {} has an invalid budget", role.name));
        }
        let mut phase_names = std::collections::HashSet::new();
        for phase in &role.phases {
            if !valid_plan_name(&phase.name) || !phase_names.insert(phase.name.as_str()) {
                return Err(format!("role {} has invalid phase names", role.name));
            }
            if !(20..=1200).contains(&phase.objective.len())
                || phase.min_tool_calls < 0
                || phase.min_tool_calls > phase.max_tool_calls
                || !(0..=32).contains(&phase.max_tool_calls)
                || !valid_capabilities(&phase.capabilities)
            {
                return Err(format!(
                    "role {} phase {} is invalid",
                    role.name, phase.name
                ));
            }
            if phase.required_tool_calls.len() > 8
                || phase.required_tool_calls.len() as i32 > phase.max_tool_calls
            {
                return Err(format!(
                    "role {} phase {} has invalid required tool calls",
                    role.name, phase.name
                ));
            }
            for call in &phase.required_tool_calls {
                if call.name != "github_actions_job_logs"
                    || !phase
                        .capabilities
                        .iter()
                        .any(|capability| capability == "mcp")
                    || ["owner", "repo", "job_id"].iter().any(|key| {
                        call.arguments
                            .get(*key)
                            .is_none_or(|value| value.trim().is_empty())
                    })
                    || call.arguments.get("tail_lines").is_some_and(|lines| {
                        lines
                            .parse::<u32>()
                            .ok()
                            .is_none_or(|value| !(1..=2000).contains(&value))
                    })
                {
                    return Err(format!(
                        "role {} phase {} has an unsupported required tool call",
                        role.name, phase.name
                    ));
                }
            }
        }
        let mut dependencies = std::collections::HashSet::new();
        if role.depends_on.iter().any(|dependency| {
            dependency == &role.name
                || !role_names.contains(dependency.as_str())
                || !dependencies.insert(dependency)
        }) {
            return Err(format!("role {} has invalid dependencies", role.name));
        }
    }
    if !(20..=1200).contains(&plan.synthesis.objective.len())
        || !(0..=32).contains(&plan.synthesis.max_tool_calls)
        || !valid_capabilities(&plan.synthesis.capabilities)
    {
        return Err("execution plan synthesis is invalid".into());
    }
    let mut deliverables = std::collections::HashSet::new();
    if plan.deliverables.len() > 16
        || plan.deliverables.iter().any(|deliverable| {
            !valid_deliverable_name(&deliverable.name)
                || !deliverables.insert(deliverable.name.as_str())
        })
    {
        return Err("execution plan deliverables are invalid".into());
    }
    execution_plan_is_acyclic(plan)
        .then_some(())
        .ok_or_else(|| "execution plan dependencies must be acyclic".into())
}

fn parse_execution_plan_result(
    json: &serde_json::Value,
) -> Result<crate::routes::tasks::ExecutionPlanDto, String> {
    let value = json
        .get("execution_plan")
        .ok_or_else(|| "execution_plan is missing".to_string())?;
    let plan = serde_json::from_value(value.clone())
        .map_err(|error| format!("execution_plan does not match the required schema: {error}"))?;
    validate_execution_plan(&plan)?;
    Ok(plan)
}

fn parse_execution_plan(
    json: &serde_json::Value,
) -> Option<crate::routes::tasks::ExecutionPlanDto> {
    parse_execution_plan_result(json).ok()
}

fn execution_plan_error_from_raw(raw: &str) -> Option<String> {
    let json =
        extract_json(raw).ok_or_else(|| "response did not contain a JSON object".to_string());
    match json {
        Ok(json) => parse_execution_plan_result(&json).err(),
        Err(error) => Some(error),
    }
}

pub(crate) fn validate_delegation(delegation: &ComposeDelegation) -> Result<(), String> {
    match delegation.mode.as_str() {
        "single-agent" if delegation.roles.is_empty() && delegation.max_parallel == 1 => Ok(()),
        "principal-specialists"
            if (2..=4).contains(&delegation.roles.len())
                && delegation.max_parallel >= 1
                && delegation.max_parallel <= delegation.roles.len() as i32
                && delegation.roles.iter().all(|role| {
                    let value = serde_json::json!({
                        "name": role.name,
                        "objective": role.objective,
                    });
                    valid_delegation_role(&value).is_some()
                }) =>
        {
            let unique = delegation
                .roles
                .iter()
                .map(|role| role.name.as_str())
                .collect::<std::collections::HashSet<_>>();
            (unique.len() == delegation.roles.len())
                .then_some(())
                .ok_or_else(|| "delegation role names must be unique".to_string())
        }
        "single-agent" => {
            Err("single-agent delegation must have no roles and max_parallel=1".into())
        }
        "principal-specialists" => Err(
            "principal-specialists delegation requires 2–4 unique valid leaf roles and a bounded max_parallel"
                .into(),
        ),
        _ => Err("delegation mode must be single-agent or principal-specialists".into()),
    }
}

pub(crate) fn delegation_budget_allocation(
    total_tokens: i64,
    role_count: usize,
) -> Result<(i64, i64), String> {
    if role_count == 0 || total_tokens < role_count as i64 {
        return Err(
            "execution plans require a positive total budget with capacity for every role".into(),
        );
    }
    Ok((total_tokens, total_tokens / role_count as i64))
}

/// A one-line, plain-language basis for recommending `deployment`, drawn from
/// the learned efficiency frontier — real accepted-outcome counts, never
/// fabricated. Falls back to a generic line if the route has no stats yet.
fn efficiency_basis(eff: &crate::routes::efficiency::EfficiencyDto, deployment: &str) -> String {
    if let Some(r) = eff.routes.iter().find(|r| r.route == deployment) {
        let acc = (r.acceptance_rate * 100.0).round() as i64;
        // Distinguish a CONFIDENT recommendation (enough runs, a real acceptance
        // rate) from the best of a sparse/weak set. Without this, a route that is
        // merely "least-bad" — e.g. 7% accepted over a handful of runs — read as a
        // glowing endorsement next to the word "Recommended", which is dishonest.
        let strong = r.runs >= 5 && r.acceptance_rate >= 0.5;
        let mut s = if strong {
            format!(
                "Best learned route — {acc}% accepted over {} run{}",
                r.runs,
                if r.runs == 1 { "" } else { "s" }
            )
        } else {
            format!(
                "Best available route so far (limited signal) — {acc}% accepted over {} run{}",
                r.runs,
                if r.runs == 1 { "" } else { "s" }
            )
        };
        if !r.harness.is_empty() {
            s.push_str(&format!(" on {}", r.harness));
        }
        // Reliability of 0% over a tiny sample is "not yet established", not a
        // meaningful "0%" — report it honestly so it doesn't read as "0% reliable".
        match (r.reliability_rate, r.reliability_k, r.reliability_samples) {
            (Some(rel), Some(k), samples) if samples >= 3 && rel > 0.0 => {
                s.push_str(&format!(
                    ", pass^{k} reliability {}%",
                    (rel * 100.0).round() as i64
                ));
            }
            (Some(_), _, samples) => {
                s.push_str(&format!(", reliability not yet established (n={samples})"));
            }
            _ => {}
        }
        if let Some(usd) = r.usd_per_outcome {
            s.push_str(&format!(", ${usd:.2}/outcome"));
        }
        s.push('.');
        s
    } else {
        "Recommended by the learned efficiency frontier.".to_string()
    }
}

/// Extract the first balanced JSON object from a string, tolerating code fences
/// and leading/trailing prose that some models add despite instructions.
fn extract_json(raw: &str) -> Option<serde_json::Value> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw.trim()) {
        return Some(v);
    }
    let bytes = raw.as_bytes();
    let start = raw.find('{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for i in start..bytes.len() {
        let c = bytes[i] as char;
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return serde_json::from_str(&raw[start..=i]).ok();
                }
            }
            _ => {}
        }
    }
    None
}

// ─── Team orchestrator: charter → org chart ──────────────────────────────────
//
// The symmetric counterpart to the mission orchestrator. From a standing-team
// charter it proposes a full org chart — a roster of member roles, each with a
// purpose-fit harness and model — informed by the SAME efficiency frontier the
// mission composer uses. This is the bread-and-butter: different roles can run
// different harnesses/models, chosen by what actually performs. A human reviews
// and edits before the team is created.

#[derive(Debug, Deserialize)]
pub struct ComposeTeamRequest {
    pub charter: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComposeTeamRole {
    pub name: String,
    pub system_prompt: String,
    /// Harness kind (validated against real wired runtimes) or empty for the
    /// team default.
    pub runtime: String,
    /// Model as `provider::deployment` (validated) or empty for team default.
    pub model: String,
    pub skills: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComposeTeamMilestone {
    pub id: String,
    pub title: String,
    pub description: String,
    pub owner_role: Option<String>,
    pub depends_on: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub review_required: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComposeTeamProposal {
    pub tier: i32,
    pub cadence_minutes: i64,
    pub instructions: String,
    /// Principal/default model as `provider::deployment`.
    pub model: String,
    /// Ordered routes that independently qualify the complete Team contract.
    pub model_fallbacks: Vec<String>,
    /// Evidence-backed reason for the principal model choice.
    pub model_basis: Option<String>,
    /// Historical cost of the selected route, when the efficiency graph has
    /// enough retained outcomes to measure it.
    pub expected_tokens_per_outcome: Option<i64>,
    pub efficiency_sample_runs: i64,
    pub mcp_servers: Vec<String>,
    pub memory: Option<String>,
    pub egress: Vec<ComposeEgress>,
    pub egress_mode: String,
    pub engineering_enabled: bool,
    pub engineering_signals: Vec<String>,
    pub engineering_poll_interval_seconds: i64,
    pub engineering_auto_run: bool,
    pub roles: Vec<ComposeTeamRole>,
    pub execution_plan: Option<crate::routes::tasks::ExecutionPlanDto>,
    pub milestones: Vec<ComposeTeamMilestone>,
}

#[derive(Debug, Serialize)]
pub struct ComposeTeamResponse {
    pub available: bool,
    pub reason: Option<String>,
    pub proposal: Option<ComposeTeamProposal>,
    pub rationale: Option<String>,
    pub source: Option<String>,
}

/// `POST /api/namespaces/:ns/compose-team` — orchestrate an org chart from a
/// charter. Same honesty contract as the mission composer: absent/failing
/// orchestrator returns `available:false` with a reason so the UI falls back to
/// manual composition.
pub async fn compose_team(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(ns): Path<String>,
    Json(req): Json<ComposeTeamRequest>,
) -> AppResult<Json<ComposeTeamResponse>> {
    let cluster = state.cluster().ok_or(AppError::ClusterUnavailable)?;
    let charter = req.charter.trim();
    if charter.len() < 8 {
        return Err(AppError::BadRequest("a real charter is required".into()));
    }

    let mut options = build_options(cluster).await?;
    options.mcp_servers.retain(|server| server.namespace == ns);
    options.memories.retain(|memory| memory.namespace == ns);
    options.skills.retain(|skill| skill.namespace == ns);
    let efficiency = crate::routes::efficiency::compute_efficiency_for_owner(
        cluster,
        Some(principal.sub.as_str()),
    )
    .await;
    let orchestrator_route = select_orchestrator_route(&options, &efficiency);
    let qualification_constraints = crate::routes::options::qualification_constraints_summary()
        .unwrap_or_else(|error| format!("  (qualification records unavailable: {error})"));
    let resource_qualification_constraints =
        crate::routes::options::resource_qualification_summary(&options).unwrap_or_else(|error| {
            format!("  (resource qualification records unavailable: {error})")
        });
    let system = build_team_system_prompt(
        &options,
        &efficiency,
        &qualification_constraints,
        &resource_qualification_constraints,
    );
    let user = format!(
        "Team charter:\n{charter}\n\nCompose the org chart now. Respond with ONLY the JSON object."
    );

    let env_orchestrator = std::env::var("BRIDGE_ORCHESTRATOR_ENDPOINT")
        .is_ok_and(|v| !v.trim().is_empty())
        && std::env::var("BRIDGE_ORCHESTRATOR_TOKEN").is_ok_and(|v| !v.trim().is_empty())
        && std::env::var("BRIDGE_ORCHESTRATOR_MODEL").is_ok_and(|v| !v.trim().is_empty());
    let pinned_orchestrator_model = cluster.bridge_orchestrator_model().await;
    let resolved_model = orchestrator_route
        .as_ref()
        .map(|(_, deployment, _)| deployment.clone())
        .or(pinned_orchestrator_model)
        .or_else(|| {
            options
                .models
                .iter()
                .find(|m| m.is_default)
                .or_else(|| options.models.first())
                .map(|m| m.deployment.clone())
        });
    if resolved_model.is_none() && !env_orchestrator {
        return Ok(Json(ComposeTeamResponse {
            available: false,
            reason: Some(
                "No inference models are configured on this cluster, so the org-composer can't run. Add an inference provider in the Operator Console, or shape the org manually below.".into(),
            ),
            proposal: None,
            rationale: None,
            source: None,
        }));
    }
    let default_model = resolved_model.unwrap_or_default();
    if !env_orchestrator
        && let Some((provider, deployment, _)) = &orchestrator_route
        && let Err(error) = cluster
            .configure_bridge_orchestrator_model(provider, deployment)
            .await
    {
        return Ok(Json(ComposeTeamResponse {
            available: false,
            reason: Some(format!(
                "The best orchestrator route ({provider}/{deployment}) could not be configured: {error}"
            )),
            proposal: None,
            rationale: None,
            source: None,
        }));
    }

    let (raw, mut source) = match orchestrator_complete(
        cluster,
        &system,
        &user,
        &default_model,
        TEAM_COMPOSE_MAX_TOKENS,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return Ok(Json(ComposeTeamResponse {
                available: false,
                reason: Some(format!(
                    "The org-composer could not compose ({e}). Shape the org manually below — the same building blocks the orchestrator would use."
                )),
                proposal: None,
                rationale: None,
                source: None,
            }));
        }
    };

    let mut execution_plan_error = execution_plan_error_from_raw(&raw);
    let (mut proposal, mut rationale) =
        parse_and_validate_team(&raw, &options, &efficiency, charter);
    if !team_proposal_is_complete(&proposal) {
        let repair_user = format!(
            "{user}\n\nYour previous response was incomplete. The exact structural problem was: {}. \
             Return the complete JSON object now; preserve a small roster of independent evidence \
             roles, make every execution-plan role name exactly match one roster role name, and do \
             not include prose or code fences.",
            incomplete_team_proposal_detail(&proposal, execution_plan_error.as_deref())
        );
        if let Ok((repair_raw, repair_source)) = orchestrator_complete(
            cluster,
            &system,
            &repair_user,
            &default_model,
            TEAM_COMPOSE_MAX_TOKENS,
        )
        .await
        {
            execution_plan_error = execution_plan_error_from_raw(&repair_raw);
            (proposal, rationale) =
                parse_and_validate_team(&repair_raw, &options, &efficiency, charter);
            source = repair_source;
        }
    }
    if !team_proposal_is_complete(&proposal) {
        return Ok(Json(ComposeTeamResponse {
            available: false,
            reason: Some(format!(
                "The org-composer returned an incomplete proposal twice (missing: {}). Shape the org manually below rather than treating generic fallback roles as an AI recommendation.",
                incomplete_team_proposal_detail(&proposal, execution_plan_error.as_deref()),
            )),
            proposal: None,
            rationale: None,
            source: Some(source),
        }));
    }
    if let Err(error) = normalize_team_proposal_route(&mut proposal, &options) {
        let repair_user = format!(
            "{user}\n\nYour previous response was not launchable: {error}\n\
             Recompose it so the principal route, every role route, and every selected MCP \
             server, memory binding, and approved skill fit retained qualification evidence at \
             the CURRENT digest. Generic route records do not prove a specific resource.\n\n\
             QUALIFIED EXECUTION RECORDS:\n{qualification_constraints}\n\n\
             RESOURCE QUALIFICATION RECORDS:\n{resource_qualification_constraints}\n\n\
             Return ONLY the complete JSON object."
        );
        if let Ok((repair_raw, repair_source)) = orchestrator_complete(
            cluster,
            &system,
            &repair_user,
            &default_model,
            TEAM_COMPOSE_MAX_TOKENS,
        )
        .await
        {
            (proposal, rationale) =
                parse_and_validate_team(&repair_raw, &options, &efficiency, charter);
            source = repair_source;
        }
    }
    if let Err(error) = normalize_team_proposal_route(&mut proposal, &options) {
        return Ok(Json(ComposeTeamResponse {
            available: false,
            reason: Some(format!(
                "The org-composer could not produce a launchable team after retry: {error}"
            )),
            proposal: None,
            rationale: None,
            source: Some(source),
        }));
    }
    proposal.model_fallbacks = qualified_team_fallbacks(&proposal, &options);
    Ok(Json(ComposeTeamResponse {
        available: true,
        reason: None,
        proposal: Some(proposal),
        rationale,
        source: Some(source),
    }))
}

fn team_proposal_is_complete(proposal: &ComposeTeamProposal) -> bool {
    !proposal.instructions.trim().is_empty()
        && !proposal.roles.is_empty()
        && proposal.execution_plan.is_some()
}

fn incomplete_team_proposal_detail(
    proposal: &ComposeTeamProposal,
    execution_plan_error: Option<&str>,
) -> String {
    let mut missing = Vec::new();
    if proposal.instructions.trim().is_empty() {
        missing.push("instructions");
    }
    if proposal.roles.is_empty() {
        missing.push("independent roles");
    }
    if proposal.execution_plan.is_none() {
        missing.push(
            execution_plan_error
                .unwrap_or("valid execution_plan with role names exactly matching the roster"),
        );
    }
    if missing.is_empty() {
        "unknown structural mismatch".into()
    } else {
        missing.join(", ")
    }
}

fn default_model_route(options: &crate::routes::options::Options) -> Option<String> {
    options
        .models
        .iter()
        .find(|model| model.is_default)
        .or_else(|| options.models.first())
        .map(|model| format!("{}::{}", model.provider, model.deployment))
}

fn team_role_qualification_requirements(
    plan: &crate::routes::tasks::ExecutionPlanDto,
    role_name: &str,
) -> std::collections::BTreeSet<String> {
    let mut required =
        std::collections::BTreeSet::from(["team".to_string(), "telemetry".to_string()]);
    if let Some(role) = plan.roles.iter().find(|role| role.name == role_name) {
        for phase in &role.phases {
            required.extend(phase.capabilities.iter().cloned());
        }
    }
    required
}

fn qualify_role_resource(
    runtime: &str,
    provider: &str,
    deployment: &str,
    route: &str,
    label: &str,
    qualified: Result<bool, String>,
    detail: impl FnOnce() -> String,
) -> Result<(), String> {
    match qualified {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!(
            "{label} lacks retained resource qualification for {route}. {}",
            detail()
        )),
        Err(error) => Err(format!(
            "{label} could not be matched against qualification records for {runtime} · {provider}::{deployment}: {error}"
        )),
    }
}

fn team_proposal_qualification(
    proposal: &ComposeTeamProposal,
    options: &crate::routes::options::Options,
) -> Result<(), String> {
    let principal_runtime = "OpenClaw";
    let principal_route = proposal
        .model
        .split_once("::")
        .map(|(provider, deployment)| (provider.to_string(), deployment.to_string()))
        .or_else(|| {
            default_model_route(options).and_then(|route| {
                route
                    .split_once("::")
                    .map(|(provider, deployment)| (provider.to_string(), deployment.to_string()))
            })
        })
        .ok_or_else(|| "the team proposal has no launchable principal model route".to_string())?;
    let principal_blueprint = crate::routes::tasks::BlueprintDto {
        runtime: Some(principal_runtime.to_string()),
        model: Some(crate::routes::tasks::ModelDto {
            provider: principal_route.0.clone(),
            deployment: principal_route.1.clone(),
        }),
        model_fallbacks: Vec::new(),
        instructions: Some(proposal.instructions.clone()),
        tool_policy: None,
        mcp_servers: proposal.mcp_servers.clone(),
        egress: proposal
            .egress
            .iter()
            .map(|entry| crate::routes::tasks::EgressDto {
                host: entry.host.clone(),
                port: entry.port.map(i32::from),
            })
            .collect(),
        egress_mode: Some(proposal.egress_mode.clone()),
        isolation: None,
        memory: proposal.memory.clone(),
        skills: proposal
            .roles
            .iter()
            .flat_map(|role| role.skills.iter().cloned())
            .collect(),
        execution_plan: proposal.execution_plan.clone(),
    };
    let (required, max_parallel) =
        crate::routes::validate::qualification_requirements(&principal_blueprint, Some("team"));
    if !crate::routes::options::route_qualification(
        principal_runtime,
        &principal_route.0,
        &principal_route.1,
        &required,
        max_parallel,
        None,
    )? {
        let missing = crate::routes::options::route_qualification_gap(
            principal_runtime,
            &principal_route.0,
            &principal_route.1,
            &required,
            max_parallel,
            None,
        )?;
        return Err(format!(
            "{} lacks retained qualification for [{}] at max_parallel={max_parallel}",
            crate::routes::options::route_label(
                principal_runtime,
                &principal_route.0,
                &principal_route.1
            ),
            missing.into_iter().collect::<Vec<_>>().join(", "),
        ));
    }
    let principal_route_label = crate::routes::options::route_label(
        principal_runtime,
        &principal_route.0,
        &principal_route.1,
    );
    for server in &proposal.mcp_servers {
        let option = option_named(&options.mcp_servers, server)
            .ok_or_else(|| format!("MCP server `{server}` is not in the live options catalogue"))?;
        qualify_role_resource(
            principal_runtime,
            &principal_route.0,
            &principal_route.1,
            &principal_route_label,
            &format!("MCP server `{server}`"),
            crate::routes::options::mcp_server_qualified_for_route(
                principal_runtime,
                &principal_route.0,
                &principal_route.1,
                option,
            ),
            || {
                format!(
                    "Current schema digest: {}. Generic route records do not prove this server.",
                    option.tool_schema_digest.as_deref().unwrap_or("missing")
                )
            },
        )?;
    }
    if let Some(memory) = proposal.memory.as_deref() {
        let option = option_named(&options.memories, memory)
            .ok_or_else(|| format!("memory `{memory}` is not in the live options catalogue"))?;
        qualify_role_resource(
            principal_runtime,
            &principal_route.0,
            &principal_route.1,
            &principal_route_label,
            &format!("memory `{memory}`"),
            crate::routes::options::memory_binding_qualified_for_route(
                principal_runtime,
                &principal_route.0,
                &principal_route.1,
                option,
            ),
            || {
                format!(
                    "Current backend/digest: {}/{}. Generic route records do not prove this memory binding.",
                    option.backend.as_deref().unwrap_or("missing"),
                    option.compiled_digest.as_deref().unwrap_or("missing")
                )
            },
        )?;
    }
    let Some(plan) = proposal.execution_plan.as_ref() else {
        return Err("the team proposal has no typed execution plan".into());
    };
    let default_role_route = format!("{}::{}", principal_route.0, principal_route.1);
    for role in &proposal.roles {
        let route = if role.model.trim().is_empty() {
            default_role_route.as_str()
        } else {
            role.model.trim()
        };
        let (provider, deployment) = route
            .split_once("::")
            .ok_or_else(|| format!("role {} has no valid model route", role.name))?;
        let runtime = if role.runtime.trim().is_empty() {
            principal_runtime
        } else {
            role.runtime.trim()
        };
        let role_required = team_role_qualification_requirements(plan, &role.name);
        if !crate::routes::options::route_qualification(
            runtime,
            provider,
            deployment,
            &role_required,
            1,
            None,
        )? {
            let missing = crate::routes::options::route_qualification_gap(
                runtime,
                provider,
                deployment,
                &role_required,
                1,
                None,
            )?;
            return Err(format!(
                "role `{}` route {} lacks retained qualification for [{}]",
                role.name,
                crate::routes::options::route_label(runtime, provider, deployment),
                missing.into_iter().collect::<Vec<_>>().join(", "),
            ));
        }
        let role_route_label = crate::routes::options::route_label(runtime, provider, deployment);
        if role_required.contains("mcp") {
            for server in &proposal.mcp_servers {
                let option = option_named(&options.mcp_servers, server).ok_or_else(|| {
                    format!("MCP server `{server}` is not in the live options catalogue")
                })?;
                qualify_role_resource(
                    runtime,
                    provider,
                    deployment,
                    &role_route_label,
                    &format!("role `{}` MCP server `{server}`", role.name),
                    crate::routes::options::mcp_server_qualified_for_route(
                        runtime, provider, deployment, option,
                    ),
                    || {
                        format!(
                            "Current schema digest: {}. Generic route records do not prove this server.",
                            option.tool_schema_digest.as_deref().unwrap_or("missing")
                        )
                    },
                )?;
            }
        }
        if role_required.contains("memory")
            && let Some(memory) = proposal.memory.as_deref()
        {
            let option = option_named(&options.memories, memory)
                .ok_or_else(|| format!("memory `{memory}` is not in the live options catalogue"))?;
            qualify_role_resource(
                runtime,
                provider,
                deployment,
                &role_route_label,
                &format!("role `{}` memory `{memory}`", role.name),
                crate::routes::options::memory_binding_qualified_for_route(
                    runtime, provider, deployment, option,
                ),
                || {
                    format!(
                        "Current backend/digest: {}/{}. Generic route records do not prove this memory binding.",
                        option.backend.as_deref().unwrap_or("missing"),
                        option.compiled_digest.as_deref().unwrap_or("missing")
                    )
                },
            )?;
        }
        for skill in &role.skills {
            let option = option_named(&options.skills, skill)
                .ok_or_else(|| format!("skill `{skill}` is not in the approved live catalogue"))?;
            qualify_role_resource(
                runtime,
                provider,
                deployment,
                &role_route_label,
                &format!("role `{}` skill `{skill}`", role.name),
                crate::routes::options::skill_version_qualified_for_route(
                    runtime, provider, deployment, option,
                ),
                || {
                    format!(
                        "Current version digest: {}. Generic route records do not prove this approved skill version.",
                        option.version_digest.as_deref().unwrap_or("missing")
                    )
                },
            )?;
        }
    }
    Ok(())
}

fn normalize_team_proposal_route(
    proposal: &mut ComposeTeamProposal,
    options: &crate::routes::options::Options,
) -> Result<(), String> {
    for role in &mut proposal.roles {
        if is_non_autonomous_harness(&role.runtime) {
            role.runtime = "OpenClaw".into();
        }
    }
    let initial_error = match team_proposal_qualification(proposal, options) {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    let original_model = proposal.model.clone();
    let original_role_routes = proposal
        .roles
        .iter()
        .map(|role| (role.runtime.clone(), role.model.clone()))
        .collect::<Vec<_>>();
    let mut candidates = Vec::new();
    if let Some(default) = default_model_route(options) {
        candidates.push(default);
    }

    candidates.extend(
        options
            .models
            .iter()
            .map(|model| format!("{}::{}", model.provider, model.deployment)),
    );
    let mut seen = std::collections::HashSet::new();
    for route in candidates
        .into_iter()
        .filter(|route| seen.insert(route.clone()))
    {
        proposal.model = route.clone();
        for role in &mut proposal.roles {
            role.runtime.clear();
            role.model.clear();
        }
        if team_proposal_qualification(proposal, options).is_ok() {
            proposal.model_basis = Some(format!(
                "Bridge selected {route} because the complete team plan and its reviewed resources match one retained qualification record; the orchestrator's proposed route did not."
            ));
            proposal.expected_tokens_per_outcome = None;
            proposal.efficiency_sample_runs = 0;
            return Ok(());
        }
    }
    proposal.model = original_model;
    for (role, (runtime, model)) in proposal.roles.iter_mut().zip(original_role_routes) {
        role.runtime = runtime;
        role.model = model;
    }
    Err(initial_error)
}

fn qualified_team_fallbacks(
    proposal: &ComposeTeamProposal,
    options: &crate::routes::options::Options,
) -> Vec<String> {
    let mut candidates = options
        .models
        .iter()
        .map(|model| format!("{}::{}", model.provider, model.deployment))
        .filter(|route| route != &proposal.model)
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.dedup();
    candidates
        .into_iter()
        .filter(|route| {
            let mut trial = proposal.clone();
            trial.model = route.clone();
            trial.model_fallbacks.clear();
            for role in &mut trial.roles {
                role.model = route.clone();
            }
            team_proposal_qualification(&trial, options).is_ok()
        })
        .take(8)
        .collect()
}

fn should_strengthen_team_principal(
    role_count: usize,
    current_deployment: &str,
    strongest_deployment: &str,
) -> bool {
    if role_count < 3 || current_deployment == strongest_deployment {
        return false;
    }
    let current = orchestrator_quality_score(current_deployment).unwrap_or(0);
    let strongest = orchestrator_quality_score(strongest_deployment).unwrap_or(0);
    current < 900 && strongest >= 950
}

fn efficient_member_route_is_qualified(runs: i64, acceptance_rate: f64) -> bool {
    // A lower-cost member route needs repeated evidence before a newly composed
    // team inherits it. This is intentionally stricter on sample count than a
    // descriptive efficiency-basis label because it changes live execution.
    runs >= 3 && acceptance_rate >= 0.67
}

/// Build the team-orchestrator system prompt. Enumerates the real harnesses +
/// models + the efficiency frontier, and asks for an org chart where roles are
/// purpose-fit and may use DIFFERENT harnesses/models per their function and
/// what the frontier shows performs.
fn build_team_system_prompt(
    o: &crate::routes::options::Options,
    eff: &crate::routes::efficiency::EfficiencyDto,
    qualification_constraints: &str,
    resource_qualification_constraints: &str,
) -> String {
    let models = o
        .models
        .iter()
        .map(|m| {
            format!(
                "  - \"{}::{}\"{}",
                m.provider,
                m.deployment,
                if m.is_default { " (default)" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let runtimes = o
        .runtimes
        .iter()
        .filter(|r| r.wired && r.kind != "BYO")
        .map(|r| format!("  - \"{}\" — {} ({})", r.kind, r.label, r.status))
        .collect::<Vec<_>>()
        .join("\n");
    let mcp_servers = if o.mcp_servers.is_empty() {
        "  (none installed)".to_string()
    } else {
        o.mcp_servers
            .iter()
            .map(|server| {
                format!(
                    "  - \"{}\"{}{}{}{}",
                    server.name,
                    server
                        .summary
                        .as_deref()
                        .map(|s| format!(" — {s}"))
                        .unwrap_or_default(),
                    if server.discovered_tools.is_empty() {
                        String::new()
                    } else {
                        format!(" · tools=[{}]", server.discovered_tools.join(", "))
                    },
                    server
                        .tool_schema_digest
                        .as_deref()
                        .map(|digest| format!(" · schema_digest={digest}"))
                        .unwrap_or_else(|| " · schema_digest=missing".into()),
                    server
                        .mode
                        .as_deref()
                        .map(|mode| format!(" · mode={mode}"))
                        .unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let memories = if o.memories.is_empty() {
        "  (none configured)".to_string()
    } else {
        o.memories
            .iter()
            .map(|memory| {
                format!(
                    "  - \"{}\"{}{}{}{}",
                    memory.name,
                    memory
                        .summary
                        .as_deref()
                        .map(|summary| format!(" — {summary}"))
                        .unwrap_or_default(),
                    memory
                        .backend
                        .as_deref()
                        .map(|backend| format!(" · backend={backend}"))
                        .unwrap_or_else(|| " · backend=missing".into()),
                    memory
                        .compiled_digest
                        .as_deref()
                        .map(|digest| format!(" · compiled_digest={digest}"))
                        .unwrap_or_else(|| " · compiled_digest=missing".into()),
                    memory
                        .readiness
                        .as_deref()
                        .map(|readiness| format!(" · readiness={readiness}"))
                        .unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let skills = if o.skills.is_empty() {
        "  (none approved)".to_string()
    } else {
        o.skills
            .iter()
            .map(|skill| {
                format!(
                    "  - \"{}\"{}{}{}{}",
                    skill.name,
                    skill
                        .summary
                        .as_deref()
                        .map(|summary| format!(" — {summary}"))
                        .unwrap_or_default(),
                    skill
                        .version
                        .as_deref()
                        .map(|version| format!(" · version={version}"))
                        .unwrap_or_default(),
                    skill
                        .version_digest
                        .as_deref()
                        .map(|digest| format!(" · version_digest={digest}"))
                        .unwrap_or_else(|| " · version_digest=missing".into()),
                    skill
                        .recipe
                        .as_deref()
                        .map(|recipe| {
                            format!(" · recipe={}", recipe.chars().take(180).collect::<String>())
                        })
                        .unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let efficiency = if eff.routes.is_empty() {
        "  (no completed runs yet — use the default model for roles unless a role clearly needs a stronger one)".to_string()
    } else {
        let mut lines = eff
            .routes
            .iter()
            .take(6)
            .map(|r| {
                format!(
                    "  - route \"{}\": {:.0}% accepted, {} tokens/outcome over {} run(s){}",
                    r.route,
                    r.acceptance_rate * 100.0,
                    r.tokens_per_outcome,
                    r.runs,
                    if !eff.recommended_low_confidence
                        && eff.recommended.as_deref() == Some(r.route.as_str())
                    {
                        "  ← recommended"
                    } else if eff.recommended_low_confidence
                        && eff.recommended.as_deref() == Some(r.route.as_str())
                    {
                        "  ← best observed, insufficient evidence"
                    } else {
                        ""
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        if eff.recommended_low_confidence {
            lines.push_str("\n  Evidence is too sparse for automatic route inheritance. Use the team default for routine roles and a stronger model only where the role's reasoning or orchestration burden clearly requires it.");
        } else {
            lines.push_str("\n  Use the frontier to assign models: give cheap/high-acceptance routes to routine roles, and a stronger model only to roles whose work demands it.");
        }
        lines
    };

    format!(
        r#"You are the kars team orchestrator. You turn a standing-team CHARTER into an org chart: a small roster of member roles that together fulfil the charter. Each role can run a DIFFERENT harness and model — choose what fits its job and what the efficiency frontier shows performs. You propose; a human reviews and edits before the team is created.

You MUST only use the harnesses and models listed below — never invent one.

HARNESSES (pick per role, or "" for the team default):
{runtimes}

MODELS (pick per role as "provider::deployment", or "" for the team default):
{models}

CONNECTED SERVICES / MCP (select only services the charter genuinely needs):
{mcp_servers}
Foundry-native web search, file search, memory, and code execution are Kars plugin tools and do not require MCP. If the customer explicitly requests an installed MCP server, select it and declare `mcp`; the complete capability combination must match one atomic qualification record.

SHARED MEMORY STORES (optional; default to a qualified Foundry-backed store when one is already configured and useful for continuity):
{memories}

APPROVED SKILLS (assign only when a role genuinely benefits from the recipe below):
{skills}

EFFICIENCY FRONTIER (learned from completed runs; honest signal is human ACCEPTANCE):
{efficiency}

QUALIFIED EXECUTION RECORDS (the full team plan MUST fit one route record; records do not compose):
{qualification_constraints}

RESOURCE QUALIFICATION RECORDS (selected MCP servers, memory bindings, and skills MUST match one current-digest record on the chosen route; generic route records do not count):
{resource_qualification_constraints}

GUIDANCE:
- Propose 2–4 focused roles (rarely more). Each role does ONE clear part of the charter.
- Produce one typed `execution_plan` whose role names exactly match the proposed roster. Define explicit dependencies, one or more bounded phases per role, and only the generic capabilities each phase requires: filesystem-read, filesystem-write, shell, network, web-search, mcp, memory. Set `min_tool_calls` to at least 1 when a phase must produce tool-backed evidence. Do not infer capabilities from role names.
- The principal owns orchestration and the final synthesis. Never propose a coordinator, editor, integrator, or synthesis-only member whose job is merely to reconcile other roles' handbacks or write the final report. Every member must collect, inspect, test, or verify independent evidence.
- Give each role a short, specific system prompt (1–2 sentences).
- Assign harness + model per role deliberately: a research/analysis role may warrant a stronger model; a routine triage/watch role should use an efficient one. Leave model/runtime "" to inherit the team default when no strong reason exists.
- For research charters, declare `web-search` on source-discovery phases and `network` on exact-URL fetch phases (or both on one combined phase) so the retained qualification stays atomic on one route.
- Select the smallest `mcp_servers` set needed by the whole team. Use the discovered tool names and schema digests above to choose the right server. A browser/UX investigator needs a browser MCP when one is installed.
- If you assign a skill, use the recipe and version digest above to justify it. If you select MCP, memory, or skills, the rationale must name the current-digest resource qualification record that makes the choice launchable.
- Select a team-default `model` for the principal; roles may override it only when their work needs a different route.
- Propose only the external `egress` hosts genuinely required by the charter. Do not invent internal/private hosts. Use `learning` for a reviewed discovery run or `strict` when the host list is complete.
- AUTONOMY TIER for the team: 1=Manual .. 5=Full. Default 3 unless the charter warrants otherwise.
- CADENCE minutes: how often the team wakes to act (0 = passive/on-demand). Pick a sensible value for the charter (e.g. 60 for hourly monitoring), else 0.
- If the charter is continuous repository maintenance, set `engineering_enabled=true`, choose the relevant signals from `dependabot_pr`, `dependabot_alert`, `code_scanning_alert`, `secret_scanning_alert`, choose a poll interval >=300 seconds, and normally set `engineering_auto_run=true`. Otherwise disable it.
- For a concrete build, launch, research campaign, migration, or other long-horizon deliverable, propose 2–8 topologically ordered `milestones`. Each milestone owns explicit acceptance criteria and may depend only on earlier milestone IDs. Set `review_required=true` at consequential handoff/release boundaries so dependent work pauses for customer approval. Use an empty milestone list only for genuinely continuous monitoring with no finite delivery.
- If you include Mermaid flowcharts in any deliverable description or rationale, quote every label containing parser-sensitive punctuation such as :, (), [], {{}}, or /.

Respond with ONLY a JSON object (no prose, no code fences) of exactly this shape:
{{
  "tier": <int 1-5>,
  "cadence_minutes": <int>,
  "instructions": "<1-2 sentence team-level mandate>",
  "model": "<provider::deployment or empty for cluster default>",
  "mcp_servers": ["<installed MCP server name>"],
  "memory": "<qualified memory name or null>",
  "egress": [{{"host":"<public DNS host>","port":443}}],
  "egress_mode": "<learning or strict>",
  "engineering_enabled": <bool>,
  "engineering_signals": ["<dependabot_pr|dependabot_alert|code_scanning_alert|secret_scanning_alert>"],
  "engineering_poll_interval_seconds": <int >=300>,
  "engineering_auto_run": <bool>,
  "roles": [
    {{"name": "<short-kebab-name>", "system_prompt": "<what this role does>", "runtime": "<harness or empty>", "model": "<provider::deployment or empty>", "skills": []}}
  ],
  "execution_plan": {{
    "schema": "kars.execution-plan/v1",
    "roles": [{{
      "name": "<exact roster role name>",
      "objective": "<role outcome>",
      "depends_on": ["<earlier role>", ...],
      "budget_tokens": <int or null>,
      "phases": [{{
        "name": "<short-kebab-phase>",
        "objective": "<phase outcome>",
        "capabilities": ["<filesystem-read|filesystem-write|shell|network|web-search|mcp|memory>", ...],
        "min_tool_calls": <int 0-32, <= max_tool_calls>,
        "max_tool_calls": <int 0-32>,
        "fresh_context": <bool>
      }}]
    }}],
    "max_parallel": <int 1-8>,
    "synthesis": {{
      "objective": "<principal synthesis outcome>",
      "capabilities": [],
      "max_tool_calls": 0
    }},
    "deliverables": []
  }},
  "milestones": [
    {{"id":"<stable-kebab-id>","title":"<milestone>","description":"<work and expected artifact>","owner_role":"<roster role or empty>","depends_on":["<earlier-id>"],"acceptance_criteria":["<verifiable condition>"],"review_required":<bool>}}
  ],
  "rationale": "<1-3 sentences explaining the org shape + key model/harness choices>"
}}"#
    )
}

fn is_synthesis_only_team_role(name: &str, system_prompt: &str) -> bool {
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
fn parse_and_validate_team(
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

#[cfg(test)]
mod capability_tests {
    use super::{
        ComposeEgress, ComposeTeamProposal, TEAM_COMPOSE_MAX_TOKENS,
        apply_weighted_role_budget_floors, build_system_prompt, build_team_system_prompt,
        catalogue_has_model_key, complete_egress_recommendation, delegation_budget_allocation,
        efficient_member_route_is_qualified, execution_plan_error_from_raw, is_autonomous_harness,
        is_non_autonomous_harness, is_synthesis_only_team_role, orchestrator_quality_score,
        parse_execution_plan, recommendation_is_actionable, should_strengthen_team_principal,
        team_proposal_is_complete, validate_execution_plan, weighted_role_budget_floors,
    };
    use crate::routes::efficiency::EfficiencyDto;
    use crate::routes::options::{IsolationOption, ModelOption, Options, RuntimeOption};

    fn test_options() -> Options {
        Options {
            models: vec![ModelOption {
                provider: "github-copilot".into(),
                deployment: "gpt-5.6-sol".into(),
                is_default: true,
                detail: None,
            }],
            default_model: Some("gpt-5.6-sol".into()),
            provider: None,
            runtimes: vec![RuntimeOption {
                kind: "OpenClaw".into(),
                label: "OpenClaw".into(),
                wired: true,
                status: "validated".into(),
                note: "ready".into(),
            }],
            isolation: vec![IsolationOption {
                value: "standard".into(),
                label: "Standard".into(),
                note: "sandboxed".into(),
            }],
            tool_policies: Vec::new(),
            mcp_servers: Vec::new(),
            mcp_profiles: Vec::new(),
            memories: Vec::new(),
            skills: Vec::new(),
        }
    }

    fn test_efficiency() -> EfficiencyDto {
        EfficiencyDto {
            routes: Vec::new(),
            recommended: None,
            recommended_harness: None,
            recommended_basis: None,
            recommended_low_confidence: false,
            total_runs: 0,
            priced: false,
        }
    }

    #[test]
    fn autonomous_set_is_openclaw_hermes_byo() {
        for k in ["OpenClaw", "openclaw", "Hermes", "hermes", "BYO", "byo"] {
            assert!(is_autonomous_harness(k), "{k} should be autonomous");
            assert!(
                !is_non_autonomous_harness(k),
                "{k} should not be non-autonomous"
            );
        }
    }

    #[test]
    fn synthesis_only_role_is_reserved_for_the_principal() {
        assert!(is_synthesis_only_team_role(
            "readiness-editor",
            "Reconcile specialist handbacks into one truthful readiness report."
        ));
        assert!(!is_synthesis_only_team_role(
            "ci-health-auditor",
            "Verify exact-head CI checks and return independent evidence."
        ));
    }

    #[test]
    fn empty_team_proposal_is_not_reported_as_available() {
        let proposal = ComposeTeamProposal {
            tier: 3,
            cadence_minutes: 0,
            instructions: String::new(),
            model: String::new(),
            model_fallbacks: Vec::new(),
            model_basis: None,
            expected_tokens_per_outcome: None,
            efficiency_sample_runs: 0,
            mcp_servers: Vec::new(),
            memory: None,
            egress: Vec::new(),
            egress_mode: "learning".into(),
            engineering_enabled: false,
            engineering_signals: Vec::new(),
            engineering_poll_interval_seconds: 900,
            engineering_auto_run: false,
            roles: Vec::new(),
            execution_plan: None,
            milestones: Vec::new(),
        };
        assert!(!team_proposal_is_complete(&proposal));
    }

    #[test]
    fn team_composer_budget_covers_full_milestone_contract() {
        const { assert!(TEAM_COMPOSE_MAX_TOKENS >= 8_192) };
    }

    #[test]
    fn collaborative_principal_uses_frontier_when_available() {
        assert!(should_strengthen_team_principal(
            3,
            "gpt-oss-120b",
            "gpt-5.6-sol"
        ));
        assert!(!should_strengthen_team_principal(
            2,
            "gpt-oss-120b",
            "gpt-5.6-sol"
        ));
        assert!(!should_strengthen_team_principal(
            3,
            "gpt-oss-120b",
            "gpt-oss-120b"
        ));
        assert!(!efficient_member_route_is_qualified(2, 1.0));
        assert!(!efficient_member_route_is_qualified(3, 0.66));
        assert!(efficient_member_route_is_qualified(3, 0.67));
    }

    #[test]
    fn sparse_route_history_does_not_drive_execution_model_selection() {
        assert!(!recommendation_is_actionable(Some("gpt-5.6-sol"), true));
        assert!(recommendation_is_actionable(Some("gpt-5.6-sol"), false));
        assert!(!recommendation_is_actionable(None, false));
    }

    #[test]
    fn bootstrap_only_adapters_are_non_autonomous() {
        for k in [
            "Anthropic",
            "OpenAIAgents",
            "MicrosoftAgentFramework",
            "LangGraph",
            "PydanticAi",
        ] {
            assert!(!is_autonomous_harness(k), "{k} should NOT be autonomous");
            assert!(is_non_autonomous_harness(k), "{k} should be non-autonomous");
        }
    }

    #[test]
    fn empty_harness_is_not_treated_as_non_autonomous() {
        // Empty = "inherit default" — must not trigger a correction.
        assert!(!is_non_autonomous_harness(""));
        assert!(!is_non_autonomous_harness("   "));
    }

    #[test]
    fn arbitrary_execution_plan_is_preserved_without_role_rewrites() {
        let value = serde_json::json!({
            "execution_plan": {
                "schema": "kars.execution-plan/v1",
                "roles": [
                    {
                        "name": "source-reader",
                        "objective": "Read the supplied source material and retain exact evidence.",
                        "depends_on": [],
                        "phases": [{
                            "name": "collect",
                            "objective": "Collect the required source evidence without synthesis.",
                            "capabilities": ["filesystem-read"],
                            "max_tool_calls": 4,
                            "fresh_context": true
                        }]
                    },
                    {
                        "name": "decision-writer",
                        "objective": "Produce the requested decision from the retained source evidence.",
                        "depends_on": ["source-reader"],
                        "phases": [{
                            "name": "draft",
                            "objective": "Draft the decision using only retained dependency evidence.",
                            "capabilities": [],
                            "max_tool_calls": 0,
                            "fresh_context": true
                        }]
                    }
                ],
                "max_parallel": 1,
                "synthesis": {
                    "objective": "Reconcile the role handbacks into the final answer.",
                    "capabilities": [],
                    "max_tool_calls": 0
                },
                "deliverables": [{"name":"decision.md","media_type":"text/markdown"}]
            }
        });
        let plan = parse_execution_plan(&value).expect("valid plan");
        assert_eq!(
            plan.roles
                .iter()
                .map(|role| role.name.as_str())
                .collect::<Vec<_>>(),
            vec!["source-reader", "decision-writer"]
        );
        assert_eq!(plan.roles[1].depends_on, vec!["source-reader"]);
    }

    #[test]
    fn execution_plan_rejects_unknown_capability_and_cycles() {
        let mut plan = parse_execution_plan(&serde_json::json!({
            "execution_plan": {
                "schema": "kars.execution-plan/v1",
                "roles": [{
                    "name": "one",
                    "objective": "Perform one arbitrary evidence task for the mission.",
                    "depends_on": [],
                    "phases": [{
                        "name": "work",
                        "objective": "Perform the arbitrary evidence task completely.",
                        "capabilities": ["shell"],
                        "max_tool_calls": 2,
                        "fresh_context": true
                    }]
                }],
                "max_parallel": 1,
                "synthesis": {
                    "objective": "Return the final mission answer from the handback.",
                    "capabilities": [],
                    "max_tool_calls": 0
                },
                "deliverables": []
            }
        }))
        .expect("valid baseline");
        plan.roles[0].phases[0].capabilities = vec!["repository-security".into()];
        assert!(validate_execution_plan(&plan).is_err());
        plan.roles[0].phases[0].capabilities = vec!["shell".into()];
        plan.roles[0].depends_on = vec!["one".into()];
        assert!(validate_execution_plan(&plan).is_err());
    }

    #[test]
    fn execution_plan_parse_error_identifies_the_exact_repair() {
        let raw = serde_json::json!({
            "execution_plan": {
                "schema": "kars.execution-plan/v1",
                "roles": [{
                    "name": "triage",
                    "objective": "Triage",
                    "phases": [{
                        "name": "inspect",
                        "objective": "Inspect the repository backlog and retain exact evidence.",
                        "capabilities": ["filesystem-read"],
                        "max_tool_calls": 1
                    }]
                }],
                "max_parallel": 1,
                "synthesis": {
                    "objective": "Present the verified maintenance recommendation to the reviewer.",
                    "capabilities": [],
                    "max_tool_calls": 0
                }
            }
        })
        .to_string();

        assert_eq!(
            execution_plan_error_from_raw(&raw).as_deref(),
            Some("role triage has an invalid objective")
        );
    }

    #[test]
    fn research_prompts_require_web_search_and_quoted_mermaid_labels() {
        let options = test_options();
        let efficiency = test_efficiency();
        let mission_prompt = build_system_prompt(&options, &efficiency, "  (none)", "  (none)");
        assert!(mission_prompt.contains("web-search"));
        assert!(mission_prompt.contains("quote every label"));

        let team_prompt = build_team_system_prompt(&options, &efficiency, "  (none)", "  (none)");
        assert!(team_prompt.contains("web-search"));
        assert!(team_prompt.contains("qualification stays atomic"));
    }

    #[test]
    fn weighted_budget_distribution_funds_scout_and_preserves_larger_explicit_roles() {
        let plan = parse_execution_plan(&serde_json::json!({
            "execution_plan": {
                "schema": "kars.execution-plan/v1",
                "roles": [
                    {
                        "name": "source-scout",
                        "objective": "Discover the authoritative URLs and fetch evidence.",
                        "depends_on": [],
                        "phases": [{
                            "name": "discover",
                            "objective": "Search and fetch the exact URLs with evidence.",
                            "capabilities": ["web-search", "network"],
                            "min_tool_calls": 1,
                            "max_tool_calls": 32,
                            "fresh_context": true
                        }]
                    },
                    {
                        "name": "analyst",
                        "objective": "Inspect the retained sources and extract facts.",
                        "depends_on": ["source-scout"],
                        "phases": [
                            {
                                "name": "inspect",
                                "objective": "Inspect the retained source bundle.",
                                "capabilities": [],
                                "max_tool_calls": 0,
                                "fresh_context": true
                            },
                            {
                                "name": "summarize",
                                "objective": "Summarize the retained evidence only.",
                                "capabilities": [],
                                "max_tool_calls": 0,
                                "fresh_context": true
                            }
                        ]
                    },
                    {
                        "name": "reporter",
                        "objective": "Draft the downstream report from retained evidence.",
                        "depends_on": ["analyst"],
                        "phases": [
                            {
                                "name": "outline",
                                "objective": "Outline the downstream report.",
                                "capabilities": [],
                                "max_tool_calls": 0,
                                "fresh_context": true
                            },
                            {
                                "name": "draft",
                                "objective": "Draft the downstream report.",
                                "capabilities": [],
                                "max_tool_calls": 0,
                                "fresh_context": true
                            }
                        ]
                    }
                ],
                "max_parallel": 1,
                "synthesis": {
                    "objective": "Return the final answer from the retained evidence.",
                    "capabilities": [],
                    "max_tool_calls": 0
                },
                "deliverables": []
            }
        }))
        .expect("valid weighted plan");
        let floors = weighted_role_budget_floors(320_000, &plan);
        assert_eq!(floors.iter().sum::<i64>(), 320_000);
        assert!(floors[0] > floors[1] * 4);
        assert_eq!(floors[1], floors[2]);

        let mut explicit = plan.clone();
        explicit.roles[1].budget_tokens = Some(90_000);
        let (changed, updated_total) = apply_weighted_role_budget_floors(&mut explicit, 320_000);
        assert!(changed);
        assert_eq!(explicit.roles[1].budget_tokens, Some(90_000));
        assert!(updated_total > 320_000);
        assert!(explicit.roles[0].budget_tokens.expect("scout budget") > 200_000);
    }

    #[test]
    fn decomposed_budget_preserves_the_parent_ceiling() {
        assert_eq!(
            delegation_budget_allocation(600_000, 3).expect("allocation"),
            (600_000, 200_000)
        );
        assert_eq!(
            delegation_budget_allocation(400_000, 3).expect("allocation"),
            (400_000, 133_333)
        );
        assert_eq!(
            delegation_budget_allocation(260_000, 3).expect("allocation"),
            (260_000, 86_666)
        );
        assert_eq!(
            delegation_budget_allocation(3, 3).expect("minimum allocation"),
            (3, 1)
        );
        assert!(delegation_budget_allocation(2, 3).is_err());
    }

    #[test]
    fn orchestration_quality_prefers_reasoning_frontier_models() {
        assert!(
            orchestrator_quality_score("gpt-5.6-sol") > orchestrator_quality_score("gpt-oss-120b")
        );
        assert!(orchestrator_quality_score("claude-opus-4.8").is_some());
        assert!(orchestrator_quality_score("text-embedding-3-small").is_none());
        assert!(orchestrator_quality_score("gpt-image-1").is_none());
    }

    #[test]
    fn model_assignment_requires_an_exact_catalogue_pair() {
        let models = vec![
            ModelOption {
                provider: "github-copilot".into(),
                deployment: "shared-name".into(),
                is_default: true,
                detail: None,
            },
            ModelOption {
                provider: "local-inference".into(),
                deployment: "local-only".into(),
                is_default: false,
                detail: None,
            },
        ];
        assert!(catalogue_has_model_key(
            &models,
            "github-copilot::shared-name"
        ));
        assert!(!catalogue_has_model_key(
            &models,
            "local-inference::shared-name"
        ));
        assert!(!catalogue_has_model_key(&models, "shared-name"));
    }

    #[test]
    fn repository_egress_is_inferred_and_provider_hosts_are_removed() {
        let result = complete_egress_recommendation(
            vec![ComposeEgress {
                host: "api.githubcopilot.com".into(),
                port: Some(443),
            }],
            "Maintain a TypeScript GitHub repository and its package.json",
            &["github".into()],
        );
        let hosts = result
            .iter()
            .map(|endpoint| endpoint.host.as_str())
            .collect::<Vec<_>>();
        assert!(!hosts.contains(&"api.githubcopilot.com"));
        assert!(hosts.contains(&"api.github.com"));
        assert!(hosts.contains(&"raw.githubusercontent.com"));
        assert!(hosts.contains(&"patch-diff.githubusercontent.com"));
        assert!(hosts.contains(&"registry.npmjs.org"));
    }

    #[test]
    fn dependabot_security_review_does_not_guess_wrong_package_registry() {
        let result = complete_egress_recommendation(
            vec![ComposeEgress {
                host: "registry.npmjs.org".into(),
                port: Some(443),
            }],
            "Review the newest Dependabot pull request and relevant security advisories",
            &["github".into()],
        );
        let hosts = result
            .iter()
            .map(|endpoint| endpoint.host.as_str())
            .collect::<Vec<_>>();
        assert!(!hosts.contains(&"registry.npmjs.org"));
        assert!(!hosts.contains(&"pypi.org"));
        assert!(hosts.contains(&"api.osv.dev"));
        assert!(hosts.contains(&"api.github.com"));
    }
}
