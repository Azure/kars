/// Build the system prompt enumerating the real building blocks + the strict
/// JSON contract. The model is told it may ONLY use these exact identifiers,
/// and is given the learned efficiency frontier so its model choice is grounded
/// in what actually performs on this cluster — not a blind pick.
pub(super) fn build_system_prompt(
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

/// Build the team-orchestrator system prompt. Enumerates the real harnesses +
/// models + the efficiency frontier, and asks for an org chart where roles are
/// purpose-fit and may use DIFFERENT harnesses/models per their function and
/// what the frontier shows performs.
pub(super) fn build_team_system_prompt(
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
