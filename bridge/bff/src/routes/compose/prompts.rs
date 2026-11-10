// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::routing::unique_model_for_deployment;

const REVIEWED_EXECUTION_CONTRACT: &str = r#"NATIVE REVIEWED EXECUTION: every new phase and synthesis MUST set execution_mode=\"native\". Describe outcomes, permissions and budgets, not a scripted tool sequence. The runtime's native agent loop chooses tool calls, source URLs and recovery within router-enforced egress, policy and reviewed funding. Ordinary phases should set min_tool_calls=0; never require filler calls. Set a positive max_tool_calls with task-appropriate headroom for useful work and retries. A phase currently runs in a separate session: set fresh_context=true; cross-phase continuity uses retained handoff documents, not conversation reuse. Declare filesystem-write for handoff exports and filesystem-read when the agent needs local or retained inputs; network enables governed public HTTPS GET and native MCP discovery/calls through the router. For native MCP work, select the installed server in mcp_servers and grant network to the phase; do not emit the unsupported phase capability `mcp`. Server bindings still require MCP route/resource qualification, tool policy and scoped credentials; network alone does not authorize arbitrary servers or tools. Let the agent discover and choose server tools rather than prescribing MCP calls in required_tool_calls. For decomposed work, synthesis requires filesystem-read and filesystem-write for handoffs and deliverables; put external evidence gathering in member phases. For single-agent Missions, use roles=[] and max_parallel=1, with the principal's entire objective, explicit native permissions and positive tool bound in synthesis. This principal may also have network for router-governed native MCP and HTTPS; grant filesystem-read for inputs/skills and filesystem-write for named deliverables. Network does NOT require predetermined source URLs: normally set required_tool_calls=[] and let the agent select URLs on the approved hosts. Use a required_tool_calls entry only for a genuine task-specific mandatory source, never merely to enable network. Such entries are completion obligations, not the exclusive allowed URLs or a prescribed order; each is {"name":"http_fetch","arguments":{"method":"GET","url":"<mandatory public HTTPS URL>"}}, without extra arguments, headers or body. Without network, required_tool_calls must be empty. Propose the task's necessary egress hosts; declarations do not themselves authorize egress. Router-denied, failed or truncated fetches return tool results for the agent to assess and recover from, not an automatic failure of ordinary research. Never claim a missing source supports a fact, or report an explicit mandatory fetch as complete when it was not. Each role, phase and synthesis objective must contain 20–1200 UTF-8 bytes, not characters; keep these outcome summaries concise and put detailed directions in the package instructions or role system_prompt. Capabilities must be unique exact names from the schema, not prose or tool names. Current retained deliverables support UTF-8 text/plain, text/markdown, text/csv or application/json. execution_plan.deliverables accepts at most 16 unique basenames, each 1–128 ASCII bytes matching ^[A-Za-z0-9][A-Za-z0-9._-]*$. Never declare response.md, constructor or prototype; these names are reserved. Runtime export locations are not deliverable names: for exports/report.md or /sandbox/exports/report.md, declare name=report.md and keep the export directory in the writing instructions. Do not silently strip arbitrary requested directories or rename reserved files; explain any unsupported requested output contract. media_type must be a supported MIME, not a prose description. Hosted web-search remains unsupported under finite funding. Explicit native memory capability enables governed persistent memory search/update in both runtimes, independently of network and inference provider. The router uses a configured/discovered Foundry project; otherwise unbound agents use Bridge's durable private built-in memory with lexical recall. A memory binding is optional: emit memory=null when no explicit configured binding is requested, never invent a Foundry resource merely because memory is needed or inference uses Azure OpenAI. Select an explicit binding only when its exact private/shared scope grants are needed; it always requires Foundry and fails closed if missing or invalid. Authorization failures and service outages never switch backends. Built-in memory is private to the exact Team/Mission owner UID and role, and is separate from retained approved knowledge. Memory deletion, arbitrary stores and implicit local memory are not granted. Queued/in_progress update status is not persistence, and completed is not human knowledge approval; require actual read-back evidence when persistence matters. Explicit native python-execute enables exact-source Python execution in both runtimes through python_execute (OpenClaw: kars_mission_python_execute), in the existing Task sandbox as that agent identity, not an extra worker or inner security boundary. It additionally requires reviewed filesystem-read, filesystem-write and network together because Python inherits that sandbox's governed identity and egress. The fixed interpreter receives only digest-checked retained inputs or files written by this run, literal argv and a bounded timeout; no package installation is performed. Actual exit status, stdout/stderr and failure/truncation/cleanup status are automatically retained in reserved kars-python-executions.json, including nonzero exits and failed Missions; do not ask the model to author that file. A tester is an ordinary role depending on application-builder, using its retained source and input artifacts; synthesis can include the original application and execution receipt in the deliverable. For decomposed work, keep execution in the tester phase; single-agent native synthesis may execute with the same explicit grants. Use approved existing proxy egress for app-made live HTTP(S) requests; fixture-backed execution or separately fetched inputs are not app-made live collection. Python inherits the agent's local identity, not upstream provider keys, and is not promised isolation from its own agent. Standalone shell and binary document extraction remain unsupported by this typed native execution path. Retain genuinely necessary unsupported capabilities and explain why that plan cannot launch; never drop requirements or misrepresent filesystem tools as external execution. Every Mission requires an explicit execution_plan, including small single-agent work: use roles=[] rather than null and do not invent a worker just to write one file."#;

const NATIVE_PEER_CONTRACT: &str = r#"NATIVE PEER COLLABORATION: native OpenClaw and Hermes worker phases with `network` can discover peers, send ordinary text, read their assignment-local inbox and await peer messages through their existing encrypted AgentMesh client. This is supported in reviewed Missions as well as Teams; a standing Team, external MCP server or new communication transport is not required just for peer exchange. Do not invent a `mesh` or `peer` capability: use `network`, retaining current assignment authority, tool policy, encryption and installed route qualification checks. The reviewed contract supplies role/phase discovery locators in peerContext; logical role labels are not registry names, and locators are not proof of current identity, readiness or permission. Let each runtime use its granted native discovery and messaging tools; do not prescribe tool calls or invent peer identities in the plan. For required live reciprocal discussion, give the participating phases overlapping execution: avoid completion dependencies between those roles and select max_parallel large enough for the simultaneous participants within installed limits. Role dependencies wait for whole-role completion; later phases and final synthesis cannot converse with already-finished workers. Keep substantive challenges, direct replies and revision within the overlapping worker phases, then synthesize their retained evidence. A send acknowledgement does not establish receipt or reply; discovery and waits may fail or time out. Never substitute simulated dialogue, sequential documents, transport counters or an empty inbox for actual attributable exchange. Preserve missing replies and disagreements in the deliverable instead of claiming consensus. Native peer support is an available execution facility, not proof that a proposed exchange has succeeded; normal pre-flight and actual run evidence remain authoritative."#;

const NATIVE_GITHUB_CONTRACT: &str = r#"NATIVE REVIEWED GITHUB PRS: both runtimes expose one bounded inspect/create adapter only with explicit `github-pr` capability in a native phase. Network/filesystem permissions and a write-enabled connection alone NEVER grant publication. Retain github-pr when PR creation is requested; select the reviewed repository-scoped GitHub binding with write=true, not raw GitHub egress or invented credentials. Inspect the exact regular UTF-8 file, then create using the observed expected_base and full replacement text. The adapter uses existing keyless router Git/REST, creates one fresh branch and commit and opens a same-repository PR; it cannot run shell, edit .git*/workflow paths, update existing/default branches, force-update, merge, delete, or accept caller branch/config/refspec arguments. This supports a single-agent principal's native synthesis or an authorized member phase; decomposed synthesis remains file-only (plus explicitly supported memory), never external publication. Retain actual upstream PR URL and commit evidence; a local report is not a published PR. Publication errors may be ambiguous: never automatically replay; retain and reconcile evidence. Existing operator branch/workflow protection, tool policy, binding readiness and installed route qualification still apply. Do not promise multi-file changes, arbitrary code execution or unsupported Git operations through this adapter."#;

const NATIVE_GITHUB_READ_CONTRACT: &str = r#"NATIVE KEYLESS GITHUB READS: both OpenClaw and Hermes can use native HTTP fetch with `network` through the existing repository-scoped GitHub App router connection. For connected private-repository metadata, prefer this route rather than adding GitHub MCP. Keep the exact repository selection for package review and require current read-only binding authorization at preflight; composition does not create a connection or grant access. Use canonical credential-free HTTPS GETs under https://api.github.com/repos/{owner}/{repo}, without custom headers or a body. Repository metadata and the paginated /branches list are supported; reconcile default_branch with the matching branch-list commit.sha when a default-branch head is needed. /branches/{branch} and /git/ref/heads/{branch} are NOT supported by this adapter. Metadata-only work must not request contents, commit details or diffs. Select no additional direct api.github.com egress for this connection: the router supplies scoped authentication, and agents never receive the App key or installation token. Do not add `github-pr` or write authority for read-only work. GitHub MCP is a separate connected service with its own bearer authorization; App repository selections neither grant nor restrict that MCP credential. Never treat successful MCP initialization or public repository reads as proof of private access. Preserve an explicit user request for MCP, but explain its separate access requirements; on denial, report the actual limitation without broadening credentials, adding raw egress or silently switching authority. This does not expand the bounded REST allowlist or permit arbitrary GitHub API routes. Decomposed synthesis remains file-only (plus explicitly supported memory); perform external reads in authorized member phases."#;

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
                if unique_model_for_deployment(&o.models, &r.route).is_none() {
                    return format!("  - deployment \"{}\" on \"{}\": no unique catalogue provider; historical measurements excluded from routing. Do not endorse a provider using this history.", r.route, r.harness);
                }
                // Structured, honest per-route signal. Absent metrics (pass^k
                // with no repeats, USD with no price table) are omitted rather
                // than faked, so the model never reasons over invented numbers.
                let reliability = match (r.reliability_rate, r.reliability_k) {
                    (Some(rate), Some(k)) => {
                        format!(", pass^{k} reliability {:.0}% (n={})", rate * 100.0, r.reliability_samples)
                    }
                    _ => String::new(),
                };
                let latency = r.avg_wall_ms.zip(r.p95_wall_ms).map(|(mean, p95)|
                    format!(", ~{:.0}s wall (p95 {:.0}s)", mean as f64 / 1000.0, p95 as f64 / 1000.0)
                ).unwrap_or_default();
                let toolfail = r.tool_fail_rate.map(|rate| format!(", {:.0}% tool-fail", rate * 100.0)).unwrap_or_default();
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
                    "  - route \"{}\" on \"{}\" ({} verified runs): {:.0}% accepted, {:.0}% delivered, {} tokens/measured delivered outcome{usd}{reliability}{latency}{toolfail}{fault} over {} run(s){}",
                    r.route, r.harness, r.verified_runs,
                    r.acceptance_rate * 100.0,
                    r.success_rate * 100.0,
                    r.tokens_per_outcome.map(|n| n.to_string()).unwrap_or_else(|| "unknown".into()),
                    r.runs,
                    if !eff.recommended_low_confidence
                        && eff.recommended.as_deref() == Some(r.route.as_str())
                        && eff.recommended_harness.as_deref() == Some(r.harness.as_str())
                    {
                        "  ← recommended"
                    } else if eff.recommended_low_confidence
                        && eff.recommended.as_deref() == Some(r.route.as_str())
                        && eff.recommended_harness.as_deref() == Some(r.harness.as_str())
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
            && unique_model_for_deployment(&o.models, rec).is_some()
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

ROUTE QUALIFICATION POLICY AND RECORDS (follow the configured policy; never claim missing evidence):
{qualification_constraints}

RESOURCE QUALIFICATION RECORDS (required mode demands current-digest evidence; explicit validation mode permits labeled unqualified tests of currently bound resources, never fabricated evidence):
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
Foundry web search, file search and code execution, plus router-selected persistent memory, are Kars plugin tools and do not require an external MCP server. Select installed MCP servers whose discovered tools fit the objective, including any explicitly requested server. For native phases, grant `network` as described below; the selected binding still requires MCP qualification. The complete capability combination and resource selection must meet the configured qualification policy.

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

EGRESS: derive the external network hosts from THIS task, as objects {{"host": "...", "port": 443}}. Public-source research requires the relevant official website/API hosts; model-only access cannot fetch those sources. Propose the smallest explicit host list that supports the work, not broad wildcard access. Respect explicit exclusions: mentioning 'no GitHub' does not request GitHub access, and writing dependency-free code does not request package-registry access. Bridge will not add missing destinations from keywords or MCP selection. Use an empty list when all access is through the model or selected governed connections, including the repository-scoped GitHub App route; those connections do not need extra direct egress hosts. Other external reads still require their reviewed hosts. Missing network qualification is a launch blocker, not a reason to omit required hosts.

INSTRUCTIONS: write a concise, specific system prompt (2–5 sentences) framing the agent's role and standards for THIS objective.

EXECUTION PLAN: when the objective benefits from decomposition, propose a workload-neutral typed execution plan. Choose arbitrary role names from the objective — never use a fixed role template. Each role has dependency-aware native phases. Normally set `min_tool_calls` to 0: judge the requested outcomes and actual evidence, not a compulsory activity count. `max_tool_calls` is the explicit upper bound.

{REVIEWED_EXECUTION_CONTRACT}

{NATIVE_PEER_CONTRACT}

{NATIVE_GITHUB_READ_CONTRACT}

{NATIVE_GITHUB_CONTRACT}

LAUNCHABILITY: follow the configured route qualification policy above for the complete capability/runtime/model/max_parallel contract. Required mode demands ONE record; never merge records. If you select an MCP server, memory binding, or approved skill in required mode, name the current-digest resource qualification record in the rationale. Explicit validation mode permits unqualified route and resource tests: state the missing evidence and label the proposal an unqualified validation run, not qualification. Approval, current bindings, authorization, budgets, tool policy and installed execution limits apply in both modes. Generic route records do not prove a specific server, backend, or skill version. Composition is not launch authorization. Prefer qualified routes that satisfy the full task, but when none exists retain the task-required capabilities, destinations, resources, roles and output formats and explain the missing qualification in the rationale. Never remove requirements or downgrade deliverables merely to fit the available records. When you emit Mermaid flowcharts, quote every label that contains parser-sensitive punctuation such as :, (), [], {{}}, or /.

RESEARCH EVIDENCE: distinguish hosted search from browsing known websites. With network permission, the native agent can retrieve official pages and follow links on approved hosts; do not predetermine article paths or claim proposed sources are verified. Set explicit mandatory GET obligations only when the task truly requires those exact sources. If the task needs hosted search rather than browsing, retain `web-search` and explain the finite-funding activation blocker. Assess returned content and provenance: never infer missing facts from a blocked, failed or incomplete page, and never treat a search snippet as a verified full source. Later phases and synthesis must ground claims in retained evidence and disclose unresolved gaps.

BUDGET: inherit workspace and ancestor policy by default, using budget_tokens=null for aggregate and role caps unless the reviewed extra spending fields specify a limit. Do not invent caps or an expiry. Null means no ADDITIONAL lifetime cap here, not unlimited funding or disabled GovernedInference accounting. Daily workspace limits are not lifetime limits and must not be divided among roles. Keep estimates separate from enforceable limits. Retain current contracts and all model, resource, security and qualification checks. Never remove or increase a reviewed finite ceiling to avoid reservation constraints. If it cannot fund the requested work, explain that blocker without dropping requirements.

MISSION ROLE ROUTING: ordinary Mission workers inherit the principal's runtime, provider and model. execution_plan.roles has no per-role runtime or model override; a role name, objective or rationale cannot configure a different route. Never claim that Mission workers use cheaper models, another provider or a different harness. If the user requires heterogeneous runtime/model roles, explain that this requires a Team with explicit per-role routing, not an ordinary Mission. Preserve that requirement as an unresolved blocker; do not silently substitute a homogeneous Mission or describe one as mixed-provider. Model fallbacks are not per-role model assignment.

MODEL FALLBACKS: leave model_fallbacks empty unless the user requests fallback routes. Preserve the requested order, use only the listed models, and never add alternatives merely because they are available.

Respond with ONLY a JSON object (no prose, no code fences) of exactly this shape:
{{
  "tier": <int 1-5>,
  "model": {{"provider": "<provider>", "deployment": "<deployment>"}},
  "model_fallbacks": [],
  "runtime": "<harness>",
  "instructions": "<system prompt>",
  "tool_policy": "<name or null>",
  "mcp_servers": ["<name>", ...],
  "skills": ["<name>", ...],
  "egress": [{{"host": "<host>", "port": <int or null>}}],
  "isolation": "<level>",
  "memory": "<name or null>",
  "budget_tokens": <reviewed extra cap as a positive safe integer, or null to inherit>,
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
        "capabilities": ["<filesystem-read|filesystem-write|shell|network|web-search|mcp|memory|github-pr>", ...],
        "required_tool_calls": [],
        "min_tool_calls": <nonnegative int32, normally 0, <= max_tool_calls>,
        "max_tool_calls": <positive int32 with task-appropriate headroom for research, tools and retries>,
        "fresh_context": true,
        "execution_mode": "native"
      }}]
    }}],
    "max_parallel": <int 1-8>,
    "synthesis": {{
      "objective": "<how the principal should reconcile handbacks>",
      "capabilities": ["<required capability>", ...],
      "max_tool_calls": <positive int32 allowing useful work and retries>,
      "execution_mode": "native"
    }},
    "deliverables": [{{"name":"<safe filename>","media_type":"<optional MIME>"}}]
  }},
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
                if unique_model_for_deployment(&o.models, &r.route).is_none() {
                    return format!("  - deployment \"{}\" on \"{}\": no unique catalogue provider; historical measurements excluded from routing. Do not endorse a provider using this history.", r.route, r.harness);
                }
                format!(
                    "  - route \"{}\" on \"{}\" ({} verified runs): {:.0}% accepted, {} tokens/measured delivered outcome over {} run(s){}",
                    r.route, r.harness, r.verified_runs,
                    r.acceptance_rate * 100.0,
                    r.tokens_per_outcome.map(|n| n.to_string()).unwrap_or_else(|| "unknown".into()),
                    r.runs,
                    if !eff.recommended_low_confidence
                        && eff.recommended.as_deref() == Some(r.route.as_str())
                        && eff.recommended_harness.as_deref() == Some(r.harness.as_str())
                    {
                        "  ← recommended"
                    } else if eff.recommended_low_confidence
                        && eff.recommended.as_deref() == Some(r.route.as_str())
                        && eff.recommended_harness.as_deref() == Some(r.harness.as_str())
                    {
                        "  ← best observed, insufficient evidence"
                    } else {
                        ""
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !eff
            .routes
            .iter()
            .any(|route| unique_model_for_deployment(&o.models, &route.route).is_some())
        {
            lines.push_str("\n  No historical deployment resolves to a unique catalogue provider. Use the team default unless the objective itself warrants another model; do not inherit models from excluded history.");
        } else if eff.recommended_low_confidence {
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
Foundry web search, file search and code execution, plus router-selected persistent memory, are Kars plugin tools and do not require an external MCP server. Select installed MCP servers whose discovered tools fit the charter, including any explicitly requested server. For native phases, grant `network` as described below; the selected binding still requires MCP qualification. The complete capability combination and resource selection must meet the configured qualification policy.

SHARED MEMORY STORES (optional; default to a qualified Foundry-backed store when one is already configured and useful for continuity):
{memories}

APPROVED SKILLS (assign only when a role genuinely benefits from the recipe below):
{skills}

EFFICIENCY FRONTIER (learned from completed runs; honest signal is human ACCEPTANCE):
{efficiency}

ROUTE QUALIFICATION POLICY AND RECORDS (follow the configured policy; never claim missing evidence):
{qualification_constraints}

RESOURCE QUALIFICATION RECORDS (required mode demands current-digest evidence; explicit validation mode permits labeled unqualified tests of currently bound resources, never fabricated evidence):
{resource_qualification_constraints}

GUIDANCE:
- Propose 2–4 focused roles (rarely more). Each role does ONE clear part of the charter.
- Produce one typed `execution_plan` whose role names exactly match the proposed roster. Define explicit dependencies, one or more bounded native phases per role, and only the exact capabilities each phase requires: filesystem-read, filesystem-write, shell, network, web-search, mcp, memory, github-pr. Normally set `min_tool_calls` to 0; do not prescribe filler calls or a fixed tool order. Do not infer capabilities from role names. If the standing Team is expected to revise reviewed deliveries or consume approved Team knowledge, explicitly propose filesystem-read and positive tool-call headroom for every consuming phase, including independent first research phases: retained revision/knowledge files reach all phases and synthesis. Explain this permission in the proposal for human review; never silently add it to an already-reviewed plan. Without it a first research run may work, but a revision must be blocked until the user reviews an updated plan.
- {REVIEWED_EXECUTION_CONTRACT} Teams require a typed plan with their actual member roles; single-agent Missions require an explicit native principal plan with roles=[].
- {NATIVE_PEER_CONTRACT}
- {NATIVE_GITHUB_READ_CONTRACT}
- {NATIVE_GITHUB_CONTRACT}
- The principal owns orchestration and the final synthesis. It is NOT a roster or execution_plan.roles entry; express its final work in execution_plan.synthesis. Never propose a coordinator, editor, integrator, or synthesis-only member whose job is merely to reconcile other roles' handbacks or write the final report. Every member must collect, inspect, test, or verify independent evidence.
- Declare every requested output file by its exact name in execution_plan.deliverables and make synthesis export those files. Mentioning filenames only in objectives or milestones does not declare an output contract. Use an empty deliverables list only when no output files are requested.
- Give each role a short, specific system prompt (1–2 sentences).
- Assign harness + model per role deliberately: a research/analysis role may warrant a stronger model; a routine triage/watch role should use an efficient one. Leave model/runtime "" to inherit the team default when no strong reason exists.
- For research charters, distinguish hosted search from browsing known websites. The native agent can select public HTTPS URLs and follow links on approved hosts with `network`; normally leave required_tool_calls empty. Never invent article paths or claim proposed URLs are verified. Retain `web-search` when hosted search is genuinely needed and explain its finite-funding activation blocker. Capabilities, egress and retained qualification stay atomic on one route. Assess actual returned content and disclose gaps; never reconstruct facts missing from failed or incomplete sources.
- Select the smallest `mcp_servers` set needed by the whole team. Use the discovered tool names and schema digests above to choose the right server. A browser/UX investigator needs a browser MCP when one is installed.
- Every roster role must include `resources` with its exact `mcp_servers` and `egress` selections, each a subset of the Team selections. Select only what that member needs, regardless of its runtime/model route. Use explicit empty arrays for a member needing neither. Member egress is Strict; do not assume global resources are copied into model/runtime overrides. Network phase capability and an explicit resource binding are both required; neither substitutes for the other. Never assign resources merely from a role name.
- If you assign a skill, use the recipe and version digest above to justify it. In required mode, name the current-digest resource qualification record for selected MCP, memory, and skills. In explicit validation mode, identify missing evidence and label the proposal an unqualified validation run. Approval, current bindings, authorization, budgets, tool policy and installed execution limits still apply.
- Select a team-default `model` for the principal; roles may override it only when their work needs a different route.
- Leave `model_fallbacks` empty unless the user requests fallback routes. Use ordered provider::deployment strings from the catalogue, not every available model.
- Derive `egress` from the charter: public-source research needs the relevant official website/API hosts, not model-only access. Propose the smallest explicit list, never invented internal/private hosts or broad wildcard access. Respect explicit exclusions: mentioning 'no GitHub' does not request GitHub access, and writing dependency-free code does not request package-registry access. Bridge will not add missing destinations from keywords or MCP selection. Always use `strict`: governed Team tasks do not support Learning. Discovery still requires reviewed hosts or explicit access approval; datapath observations do not grant access.
- Composition is not launch authorization. Prefer qualified routes that satisfy the full charter. If none exists, retain the task-required capabilities, destinations, resources, roles and deliverables and explain missing qualification in the rationale; never remove requirements to fit records.
- Inherit workspace policy and the selected live profile by default. Use `budget_tokens=null` for aggregate and roles unless the reviewed extra spending fields or a finite profile ceiling specify a limit. Never invent per-role allocations, finite floors or an expiry from estimates. Null means no ADDITIONAL cap; role usage still shares the Team UID lifetime allowance, not fresh funding per run. Daily workspace limits are separate and must not be converted into Team-lifetime caps. Retain GovernedInference accounting, current contracts, currency caps and all model, resource, security and qualification checks. Preserve smaller reviewed user limits and explain insufficient reservation capacity without removing caps or dropping requirements. Estimates belong in the rationale, clearly distinguished from enforceable limits and funding approval.
- Dependencies are role-level: EVERY phase of a predecessor finishes before any phase of its dependent role starts. An analyst phase cannot consume feedback from a reviewer that depends on that analyst. Put post-review incorporation and final file validation in the principal synthesis, which runs after all members; never invent an interleaving the scheduler cannot execute.
- AUTONOMY TIER for the team: 1=Manual .. 5=Full. Default 3 unless the charter warrants otherwise.
- CADENCE minutes: how often the team wakes to act (0 = passive/on-demand). Pick a sensible value for the charter (e.g. 60 for hourly monitoring), else 0.
- If the charter is continuous repository maintenance, set `engineering_enabled=true`, choose the relevant signals from `dependabot_pr`, `dependabot_alert`, `code_scanning_alert`, `secret_scanning_alert`, choose a poll interval >=300 seconds, and normally set `engineering_auto_run=true`. Otherwise disable it.
- Milestones are independently deliverable Team tasks, not internal role phases: each milestone runs the complete execution_plan, including all members and synthesis. Use one milestone for one requested delivery; do not split its draft/review/finalize stages into repeated whole-Team runs. For genuinely distinct long-horizon deliverables, propose up to eight topologically ordered milestones, each with explicit acceptance criteria and dependencies only on earlier IDs. Set review_required=true where customer approval must gate dependent work. Use no milestones for an on-demand charter without an initial task or continuous monitoring without a finite delivery.
- If you include Mermaid flowcharts in any deliverable description or rationale, quote every label containing parser-sensitive punctuation such as :, (), [], {{}}, or /.

Respond with ONLY a JSON object (no prose, no code fences) of exactly this shape:
{{
  "profile": <installed Team profile name as a JSON string, or null>,
  "tier": <int 1-5>,
  "cadence_minutes": <int>,
  "budget_tokens": <reviewed extra or profile Team-lifetime cap, or null to inherit>,
  "instructions": "<1-2 sentence team-level mandate>",
  "model": "<provider::deployment or empty for cluster default>",
  "model_fallbacks": [],
  "mcp_servers": ["<installed MCP server name>"],
  "memory": "<qualified memory name or null>",
  "egress": [{{"host":"<public DNS host>","port":443}}],
  "egress_mode": "strict",
  "engineering_enabled": <bool>,
  "engineering_signals": ["<dependabot_pr|dependabot_alert|code_scanning_alert|secret_scanning_alert>"],
  "engineering_poll_interval_seconds": <int >=300>,
  "engineering_auto_run": <bool>,
  "roles": [
    {{"name": "<short-kebab-name>", "system_prompt": "<what this role does>", "runtime": "<harness or empty>", "model": "<provider::deployment or empty>", "skills": [], "resources": {{"mcp_servers": ["<selected Team MCP server>"], "egress": [{{"host":"<selected Team host>","port":443}}]}}}}
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
        "capabilities": ["<filesystem-read|filesystem-write|shell|network|web-search|mcp|memory|github-pr>", ...],
        "required_tool_calls": [],
        "min_tool_calls": <nonnegative int32, normally 0, <= max_tool_calls>,
        "max_tool_calls": <positive int32 with task-appropriate headroom for research, tools and retries>,
        "fresh_context": true,
        "execution_mode": "native"
      }}]
    }}],
    "max_parallel": <int 1-8>,
    "synthesis": {{
      "objective": "<principal synthesis outcome>",
      "capabilities": ["<required capability>", ...],
      "max_tool_calls": <positive int32 allowing useful work and retries>,
      "execution_mode": "native"
    }},
    "deliverables": [{{"name":"<exact requested safe filename>","media_type":"<optional MIME>"}}]
  }},
  "milestones": [
    {{"id":"<stable-kebab-id>","title":"<milestone>","description":"<work and expected artifact>","owner_role":"<roster role or empty>","depends_on":["<earlier-id>"],"acceptance_criteria":["<verifiable condition>"],"review_required":<bool>}}
  ],
  "rationale": "<1-3 sentences explaining the org shape + key model/harness choices>"
}}"#
    )
}
