// kars Bridge BFF — launch options palette.

use axum::Json;
use axum::extract::State;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

use super::{
    IsolationOption, McpProfileOption, ModelOption, Options, ProviderInfo, RefOption,
    RuntimeOption, mcp_server_option, memory_option, name_of, ns_of, skill_option,
};

fn require_cluster(state: &AppState) -> AppResult<&crate::kars::cluster::Cluster> {
    state.cluster().ok_or(AppError::ClusterUnavailable)
}

fn upstream(e: kube::Error) -> AppError {
    AppError::Upstream(e.to_string())
}

/// Infer a provider tag from a deployment string when none is recorded — a
/// `github-models`-style `openai/<model>` carries its vendor in the prefix. A
/// bare deployment name (the Copilot/Foundry form) carries no vendor, so we tag
/// it with the cluster's ACTUAL inherited provider (`github-copilot`,
/// `github-models`, or a Foundry/Azure provider) rather than guessing
/// `azure-openai`. This is what makes a composed mission/team stamp the real
/// provider the router serves — never a misleading default.
pub(crate) fn provider_for(
    deployment: &str,
    recorded: Option<&str>,
    cluster_default: Option<&str>,
) -> String {
    if let Some(p) = recorded {
        return p.to_string();
    }
    match deployment.split_once('/') {
        Some(("openai", _)) => "github-models".to_string(),
        Some((vendor, _)) => vendor.to_string(),
        None => cluster_default.unwrap_or("azure-openai").to_string(),
    }
}

/// `GET /api/options` — the composable launch-package palette, from live state.
pub async fn get_options(State(state): State<AppState>) -> AppResult<Json<Options>> {
    let cluster = require_cluster(&state)?;
    Ok(Json(build_options(cluster).await?))
}

/// Build the real composable building blocks from live cluster state. Shared by
/// the `/api/options` route and the orchestrator (`/compose`), so the LLM only
/// ever proposes models, tool policies, MCP servers, isolation levels, and
/// memory stores that genuinely exist on this cluster.
pub async fn build_options(cluster: &crate::kars::cluster::Cluster) -> AppResult<Options> {
    // Models: the controller-configured default + catalog, deduped against any
    // distinct models already pinned on existing InferencePolicies (real,
    // in-use facts). Order: default first, then catalog, then discovered.
    let (default_model, catalog) = cluster.controller_models().await;
    // The cluster's inherited inference provider — the authoritative tag for any
    // catalog model that doesn't carry its own vendor prefix. Fetched up front
    // so every offered model is stamped with the provider the router actually
    // serves (e.g. `github-copilot`), not a neutral guess.
    let provider = cluster
        .controller_provider()
        .await
        .map(|(id, label, note)| ProviderInfo { id, label, note });
    let cluster_provider_id: Option<String> = provider.as_ref().map(|p| p.id.clone());
    // The operator's declared, served set — the only models we trust enough to
    // offer. Discovered InferencePolicy models are surfaced ONLY if they are
    // also in this set, so a stale policy pinning an unserved model (e.g. a
    // decommissioned deployment) can't leak a broken option into the picker.
    let catalog_set: std::collections::BTreeSet<String> = catalog
        .iter()
        .cloned()
        .chain(default_model.clone())
        .collect();
    let mut models: Vec<ModelOption> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // Detail blurbs (deployment → "vendor · ctx · category") for models whose
    // provider exposes them; applied in a post-pass so push_model stays simple.
    let mut details: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    let cpid = cluster_provider_id.clone();
    let mut push_model = |deployment: String, provider: Option<&str>, is_default: bool| {
        if deployment.is_empty() || !seen.insert(deployment.clone()) {
            return;
        }
        let provider = provider_for(&deployment, provider, cpid.as_deref());
        models.push(ModelOption {
            provider,
            deployment,
            is_default,
            detail: None,
        });
    };
    if let Some(def) = default_model.clone() {
        push_model(def, None, true);
    }
    for dep in catalog {
        push_model(dep, None, false);
    }
    // Live GitHub Copilot catalog — when Copilot is the cluster's default
    // provider, surface the seat's ACTUAL served models (gpt-5.6, opus-4.8,
    // gemini-3.1-pro, …) instead of only the static KARS_MODEL_CATALOG. This
    // is the SAME set the wizard's Copilot discovery shows, so the Model
    // catalogue, the orchestrator's menu, and the manual-override picker all
    // reflect what Copilot really serves — refreshing itself as GitHub adds
    // models. Cached (5-min TTL); a transient Copilot failure leaves the
    // static catalog intact (best-effort, never blanks the list).
    if cluster_provider_id.as_deref() == Some("github-copilot")
        && let Some(token) = cluster.controller_copilot_token().await
    {
        for (dep, _recommended, detail) in
            crate::routes::operator::copilot_catalog_cached(&token).await
        {
            if let Some(d) = detail {
                details.entry(dep.clone()).or_insert(d);
            }
            push_model(dep, Some("github-copilot"), false);
        }
    }
    for ip in cluster
        .list_kind_all("InferencePolicy")
        .await
        .map_err(upstream)?
    {
        let primary = ip
            .data
            .get("spec")
            .and_then(|s| s.get("modelPreference"))
            .and_then(|m| m.get("primary"));
        if let Some(p) = primary {
            let dep = p.get("deployment").and_then(|d| d.as_str()).unwrap_or("");
            // Only surface a discovered model if the operator's catalog declares
            // it — never an arbitrary (possibly unserved) pinned deployment.
            if !catalog_set.contains(dep) {
                continue;
            }
            let prov = p.get("provider").and_then(|d| d.as_str());
            push_model(dep.to_string(), prov, false);
        }
    }

    // Additional providers (§ inference-provider-wizard): every model the
    // operator explicitly declared when connecting a provider beyond the
    // single default — e.g. a Foundry deployment or a GitHub Models id —
    // tagged with ITS OWN provider, not the cluster default. These are
    // operator-declared (trusted) the same way the default catalog is, so
    // they don't need the catalog_set gate above. This is what lets
    // InferencePolicy's model picker offer "gpt-4.1 via Foundry" alongside
    // "opus-4.8 via GitHub Copilot" with no change to that editor — it
    // already keys options by `provider::deployment`.
    let provider_keys = cluster
        .read_secret_all("kars-system", "kars-inference-providers")
        .await
        .map_err(upstream)?;
    let mut declared_models: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for (key, value) in &provider_keys {
        let Some(tag_part) = key
            .strip_prefix("KARS_PROVIDER_")
            .and_then(|r| r.strip_suffix("_MODELS"))
        else {
            continue;
        };
        let tag = tag_part.to_ascii_lowercase().replace('_', "-");
        declared_models.insert(
            tag,
            value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
        );
    }
    for (tag, deployments) in declared_models {
        for dep in deployments {
            push_model(dep, Some(tag.as_str()), false);
        }
    }

    // Apply detail blurbs (only some providers expose them).
    if !details.is_empty() {
        for m in models.iter_mut() {
            if m.detail.is_none()
                && let Some(d) = details.get(&m.deployment)
            {
                m.detail = Some(d.clone());
            }
        }
    }

    // Runtimes: the controller wires adapters for several harnesses, but a
    // harness is only RUNNABLE here when its container image is configured and
    // the controller has registry credentials that cover the image.
    // `wired` now means "can start a pod here" — so the UI never lets a user (or
    // the orchestrator) pick a harness that would ErrImagePull. `status`:
    //   ready       = runnable on this cluster
    //   needs_image = adapter exists, but no image configured here
    //   unavailable = no adapter at all (SemanticKernel)
    let runnable = cluster.runnable_runtimes().await;
    let mk = |kind: &str, label: &str, ready_note: &str| {
        let is_runnable = runnable.contains(kind);
        RuntimeOption {
            kind: kind.into(),
            label: label.into(),
            wired: is_runnable,
            status: if is_runnable {
                "ready".into()
            } else {
                "needs_image".into()
            },
            note: if is_runnable {
                ready_note.into()
            } else {
                "Adapter wired, but its runtime image or registry pull credential is unavailable on this cluster.".into()
            },
        }
    };
    let runtimes = vec![
        mk(
            "OpenClaw",
            "OpenClaw",
            "Autonomous — the default kars harness, exercised end-to-end (full mesh + spawn). Runs missions and standing teams.",
        ),
        mk(
            "Hermes",
            "Hermes (Nous Research)",
            "Autonomous — executes a delivered objective in-process and replies (plugins, 20+ channels, native MCP). Verified end-to-end.",
        ),
        mk(
            "Anthropic",
            "Anthropic Claude Agent SDK",
            "Adapter only (pins the governed router) — you supply the agent logic. Not a turnkey autonomous harness; auto-corrected to OpenClaw for missions/teams.",
        ),
        mk(
            "OpenAIAgents",
            "OpenAI Agents SDK",
            "Adapter only (routes through the inference sidecar) — you supply the agent logic. Not turnkey autonomous; auto-corrected to OpenClaw for missions/teams.",
        ),
        mk(
            "MicrosoftAgentFramework",
            "Microsoft Agent Framework",
            "Adapter only (MAF Python, first-party AGT integration) — you supply the agent logic. Not turnkey autonomous; auto-corrected to OpenClaw.",
        ),
        mk(
            "LangGraph",
            "LangGraph",
            "Adapter only (Python + TypeScript, pins the router) — you supply the graph. Not turnkey autonomous; auto-corrected to OpenClaw.",
        ),
        mk(
            "PydanticAi",
            "Pydantic-AI",
            "Adapter only (provider-agnostic, pins the router at bootstrap) — you supply the agent. Not turnkey autonomous; auto-corrected to OpenClaw.",
        ),
        mk(
            "BYO",
            "Bring-your-own runtime",
            "Autonomous by contract — any image honoring the BYO contract (UID 1000, router at 127.0.0.1:8443, consumes the objective + delivers).",
        ),
        RuntimeOption {
            kind: "SemanticKernel".into(),
            label: "Semantic Kernel".into(),
            wired: false,
            status: "unavailable".into(),
            note: "Declared on the substrate but no adapter is wired yet.".into(),
        },
    ];

    let isolation = vec![
        IsolationOption {
            value: "standard".into(),
            label: "Standard".into(),
            note: "Namespaced sandbox, default-deny egress, seccomp.".into(),
        },
        IsolationOption {
            value: "enhanced".into(),
            label: "Enhanced".into(),
            note: "Hardened profile for sensitive work.".into(),
        },
        IsolationOption {
            value: "confidential".into(),
            label: "Confidential".into(),
            note: "Confidential compute (CVM) where the node pool supports it.".into(),
        },
    ];

    let tool_policies = cluster
        .list_kind_all("ToolPolicy")
        .await
        .map_err(upstream)?
        .iter()
        .map(|o| RefOption {
            name: name_of(o),
            namespace: ns_of(o),
            summary: o
                .data
                .get("spec")
                .and_then(|s| s.get("appliesTo"))
                .and_then(|a| a.get("tool"))
                .and_then(|t| t.as_str())
                .map(|t| format!("tools {t}")),
            mode: None,
            discovered_tools: Vec::new(),
            tool_schema_digest: None,
            compiled_digest: None,
            backend: None,
            readiness: None,
            version: None,
            recipe: None,
            version_digest: None,
            qualified_routes: Vec::new(),
        })
        .collect();

    let mut mcp_servers: Vec<RefOption> = cluster
        .list_kind_all("McpServer")
        .await
        .map_err(upstream)?
        .iter()
        .map(mcp_server_option)
        .collect();
    // Collapse duplicate registrations in the SAME namespace that point at the
    // same endpoint URL. Identical endpoints in different namespaces are
    // distinct workspace grants and must survive namespace filtering.
    // the audit saw two identical Playwright servers offered side by side, which
    // is confusing and invites a redundant grant. Keep the first per URL; servers
    // with no URL are always kept (nothing to compare on).
    {
        let mut seen_urls: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        mcp_servers.retain(|s| match s.summary.as_deref() {
            Some(url) if !url.is_empty() => {
                seen_urls.insert((s.namespace.clone(), url.to_string()))
            }
            _ => true,
        });
    }

    let memories = cluster
        .list_kind_all("KarsMemory")
        .await
        .map_err(upstream)?
        .iter()
        .map(memory_option)
        .collect();

    let skills = cluster
        .list_kind_all("KarsSkill")
        .await
        .map_err(upstream)?
        .iter()
        // Operator trust gate: users may only assign skills an operator has
        // approved AND locked to the skill's current version digest. A pending,
        // never-approved, or changed-since-approval skill is withheld until
        // (re)approved — the same rule the operator console enforces.
        .filter(|o| {
            let review = o
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get("kars.azure.com/skill-review"))
                .map(String::as_str);
            let locked = o
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get("kars.azure.com/skill-locked-digest"));
            let digest = o
                .data
                .get("status")
                .and_then(|s| s.get("versionDigest"))
                .and_then(|d| d.as_str());
            review == Some("approved") && locked.is_some() && locked.map(String::as_str) == digest
        })
        .map(skill_option)
        .collect();

    // Operator-curated MCP profiles (vetted bundles). Only surface servers that
    // still exist on the cluster, so a deleted McpServer can't linger in a bundle.
    let known_servers: std::collections::BTreeSet<String> =
        mcp_servers.iter().map(|r| r.name.clone()).collect();
    let mcp_profiles: Vec<McpProfileOption> = {
        let raw = cluster.read_mcp_profiles().await;
        let parsed: Vec<crate::routes::operator::McpProfileDto> =
            serde_json::from_str(&raw).unwrap_or_default();
        parsed
            .into_iter()
            .map(|p| McpProfileOption {
                name: p.name,
                summary: p.summary,
                servers: p
                    .servers
                    .into_iter()
                    .filter(|s| known_servers.contains(s))
                    .collect(),
            })
            .collect()
    };

    Ok(Options {
        models,
        default_model,
        provider,
        runtimes,
        isolation,
        tool_policies,
        mcp_servers,
        mcp_profiles,
        memories,
        skills,
    })
}
