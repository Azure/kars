use super::client::extract_json;
use super::egress::complete_egress_recommendation;
use super::execution::{
    delegation_from_execution_plan, parse_execution_plan, single_agent_delegation,
};
use super::routing::{efficiency_basis, recommendation_is_actionable};
use super::{
    ComposeEgress, ComposeModel, ComposeProposal, default_tool_policy, is_non_autonomous_harness,
};

/// validate every field against the real options — the server is the authority,
/// not the model. Anything invalid is dropped or normalized to a safe default.
pub(super) fn parse_and_validate(
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
