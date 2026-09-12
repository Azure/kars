"use client";

// kars Bridge — Team composer. Intent → a visual org chart. The user gives a
// charter; the composer proposes a principal + roles, each an editable node with
// its own harness, model, and skills (different members can run on different
// harnesses/models). This makes the team-orchestration flow legible: you SEE the
// org being designed before it is created. The roster is submitted with the team.

import { useActionState, useEffect, useMemo, useRef, useState } from "react";
import { createTeamAction, composeTeamAction, type NewTeamState } from "./actions";
import { RepoAccess } from "@/components/repo-access";
import type {
  ComposeTeamMilestone,
  ExecutionPlan,
  Options,
  ProfileSummary,
  TeamLifecycleMode,
  ValidationResult,
} from "@/lib/types";
import { PreflightCheck } from "@/components/preflight-check";
import { OrchestrationCube } from "@/components/orchestration-cube";
import { LoopDesigner } from "@/components/loop-designer";
import { Icon, type IconName } from "@/components/icon";
import { MEMBER_ARCHETYPES } from "@/lib/member-archetypes";
import { renderGovernancePanel, renderOrgPanel } from "./team-composer-panels";
import type { Role } from "./team-composer-panel-types";

const init: NewTeamState = { error: null };

let RID = 1;

function proposeRoles(): Omit<Role, "id">[] {
  return [];
}

function parseEgressLines(value: string): { host: string; port?: number }[] {
  return value
    .split(/\r?\n|,/)
    .map((entry) => entry.trim())
    .filter(Boolean)
    .flatMap((entry) => {
      const match = entry.match(/^([^:]+?)(?::(\d{1,5}))?$/);
      if (!match) return [];
      const port = match[2] ? Number(match[2]) : undefined;
      if (port != null && (port < 1 || port > 65535)) return [];
      return [{ host: match[1].toLowerCase(), ...(port ? { port } : {}) }];
    });
}

export function TeamComposer({ options, profile, initialCharter }: { options: Options; profile?: ProfileSummary | null; initialCharter?: string }) {
  const availableSkillNames = useMemo(
    () => new Set(options.skills.map((skill) => skill.name)),
    [options.skills],
  );
  const availableMcpNames = useMemo(
    () => new Set(options.mcp_servers.map((server) => server.name)),
    [options.mcp_servers],
  );
  const availableMemoryNames = useMemo(
    () => new Set(options.memories.map((entry) => entry.name)),
    [options.memories],
  );
  const [state, action, pending] = useActionState(createTeamAction, init);
  const [name, setName] = useState("");
  const [displayName, setDisplayName] = useState(profile?.display_name ?? "");
  const [charter, setCharter] = useState(profile?.charter_template ?? initialCharter ?? "");
  const [tier, setTier] = useState(profile?.tier ?? 3);
  const [addNote, setAddNote] = useState<string | null>(null);
  useEffect(() => { if (!addNote) return; const t = setTimeout(() => setAddNote(null), 2500); return () => clearTimeout(t); }, [addNote]);
  const [cadence, setCadence] = useState(0);
  const [lifecycleMode, setLifecycleMode] = useState<TeamLifecycleMode>("resourceOptimized");
  const [warmIdleMinutes, setWarmIdleMinutes] = useState(15);
  const [reporting, setReporting] = useState("");
  const [toolPolicy, setToolPolicy] = useState(profile?.tool_policy ?? "");
  const [runtime, setRuntime] = useState("");
  const [model, setModel] = useState("");
  const [modelFallbacks, setModelFallbacks] = useState<string[]>([]);
  const [mcp, setMcp] = useState<string[]>([]);
  const [memory, setMemory] = useState("");
  const [egressMode, setEgressMode] = useState<"learning" | "strict">("learning");
  const [egressText, setEgressText] = useState("");
  const [commons, setCommons] = useState(profile?.knowledge_commons ?? "");
  const [engineeringEnabled, setEngineeringEnabled] = useState(false);
  const [engineeringSignals, setEngineeringSignals] = useState<Set<string>>(new Set());
  const [engineeringPoll, setEngineeringPoll] = useState(900);
  const [engineeringAutoRun, setEngineeringAutoRun] = useState(true);
  const [selectedRepos, setSelectedRepos] = useState<string[]>([]);
  const [roles, setRoles] = useState<Role[]>(
    () =>
      profile?.roles.map((r) => ({
        id: RID++,
        name: r.name,
        system_prompt: r.system_prompt ?? "",
        runtime: "",
        model: "",
        skills: (r.skills ?? []).filter((skill) => availableSkillNames.has(skill)),
      })) ?? [],
  );
  const [milestones, setMilestones] = useState<ComposeTeamMilestone[]>([]);
  const [executionPlan, setExecutionPlan] = useState<ExecutionPlan | null>(null);
  const [executionPlanDraft, setExecutionPlanDraft] = useState("");
  const [executionPlanError, setExecutionPlanError] = useState<string | null>(null);
  const [validation, setValidation] = useState<ValidationResult | null>(null);
  const [validatedFingerprint, setValidatedFingerprint] = useState<string | null>(null);
  const [launch, setLaunch] = useState(false);
  // The team's effective package for the shared pre-flight — the same blueprint
  // shape a mission validates. The controller defaults toolPolicy to kars-default
  // and the model to the controller default, so we validate those effective
  // values. Roster-level model overrides ride on top per member.
  const teamBlueprint = useMemo(() => {
    const roleModel = roles.map((r) => r.model).find((m) => m && m.includes("::"));
    const effectiveModel = model.includes("::") ? model : roleModel;
    const modelRoute = effectiveModel
      ? { provider: effectiveModel.split("::")[0], deployment: effectiveModel.split("::")[1] }
      : null;
    return {
      runtime: runtime.trim() || undefined,
      tool_policy: toolPolicy.trim() || "kars-default",
      model: modelRoute,
      model_fallbacks: modelFallbacks.map((route) => {
        const [provider, deployment] = route.split("::");
        return { provider, deployment };
      }),
      mcp_servers: mcp,
      memory: memory.trim() || undefined,
      skills: [...new Set(roles.flatMap((role) => role.skills))],
      egress: parseEgressLines(egressText),
      egress_mode: egressMode,
      execution_plan: executionPlan,
    };
  }, [runtime, toolPolicy, roles, mcp, memory, model, modelFallbacks, egressText, egressMode, executionPlan]);
  const teamFingerprint = useMemo(
    () => JSON.stringify({ blueprint: teamBlueprint, tier }),
    [teamBlueprint, tier],
  );
  const currentValidation = validatedFingerprint === teamFingerprint ? validation : null;
  const selectedMemoryOption = useMemo(
    () => options.memories.find((entry) => entry.name === memory) ?? null,
    [memory, options.memories],
  );
  // When instantiating from a profile, skip the charter-intent step and go
  // straight to the editable org chart (everything is already prefilled).
  const [composed, setComposed] = useState(!!profile);
  const [composing, setComposing] = useState(false);
  const [rationale, setRationale] = useState<string | null>(
    profile ? `Prefilled from the “${profile.display_name ?? profile.name}” profile (${profile.domain ?? "team"} domain). Edit anything before you create.` : null,
  );
  const [modelBasis, setModelBasis] = useState<string | null>(null);
  const [expectedTokens, setExpectedTokens] = useState<number | null>(null);
  const [efficiencyRuns, setEfficiencyRuns] = useState(0);
  const [composeNote, setComposeNote] = useState<string | null>(null);

  async function compose() {
    if (charter.trim().length < 8) return;
    setComposing(true);
    setRationale(null);
    setModelBasis(null);
    setExpectedTokens(null);
    setEfficiencyRuns(0);
    setComposeNote(null);
    try {
      const res = await composeTeamAction(charter.trim());
      if (res.available && res.proposal) {
        // AI orchestrator composed the org — adopt its roster + team settings.
        const p = res.proposal;
        setTier(p.tier);
        setCadence(p.cadence_minutes);
        setModel(p.model ?? "");
        setModelFallbacks(p.model_fallbacks ?? []);
        setMcp((p.mcp_servers ?? []).filter((server) => availableMcpNames.has(server)));
        setMemory(p.memory && availableMemoryNames.has(p.memory) ? p.memory : "");
        setEgressMode(p.egress_mode ?? "learning");
        setEgressText(
          (p.egress ?? [])
            .map((entry) => `${entry.host}${entry.port ? `:${entry.port}` : ""}`)
            .join("\n"),
        );
        setEngineeringEnabled(p.engineering_enabled);
        setEngineeringSignals(new Set(p.engineering_signals ?? []));
        setEngineeringPoll(p.engineering_poll_interval_seconds ?? 900);
        setEngineeringAutoRun(p.engineering_auto_run ?? true);
        setExecutionPlan(p.execution_plan);
        setExecutionPlanDraft(p.execution_plan ? JSON.stringify(p.execution_plan, null, 2) : "");
        setExecutionPlanError(null);
        const proposedRoles = p.roles.filter((role) => role.name.trim().toLowerCase() !== "principal");
        const unavailableSkillCount = proposedRoles.reduce(
          (count, role) =>
            count + (role.skills ?? []).filter((skill) => !availableSkillNames.has(skill)).length,
          0,
        );
        setRoles(
          (proposedRoles.length ? proposedRoles : proposeRoles()).map((r) => ({
            id: RID++,
            name: r.name,
            system_prompt: r.system_prompt,
            runtime: "runtime" in r ? r.runtime : "",
            model: "model" in r ? r.model : "",
            skills: (r.skills ?? []).filter((skill) => availableSkillNames.has(skill)),
          })),
        );
        if (unavailableSkillCount > 0) {
          setComposeNote(
            `${unavailableSkillCount} suggested skill${unavailableSkillCount === 1 ? "" : "s"} ` +
              "were not in the live attested catalogue and were left out.",
          );
        }
        setMilestones(p.milestones ?? []);
        setRationale(res.rationale ?? null);
        setModelBasis(p.model_basis ?? null);
        setExpectedTokens(p.expected_tokens_per_outcome ?? null);
        setEfficiencyRuns(p.efficiency_sample_runs ?? 0);
      } else {
        // Honest fallback to the heuristic starter roster.
        setComposeNote(res.reason ?? null);
        setRoles(proposeRoles().map((r) => ({ ...r, id: RID++ })));
        setExecutionPlan(null);
        setExecutionPlanDraft("");
        setMilestones([]);
        if (/dependabot|code quality|code scanning|security finding|repository maintenance/i.test(charter)) {
          setEngineeringEnabled(true);
          setEngineeringSignals(
            new Set([
              "dependabot_pr",
              "dependabot_alert",
              "code_scanning_alert",
              "secret_scanning_alert",
            ]),
          );
          setEngineeringAutoRun(true);
        }
      }
    } catch {
      setComposeNote("Couldn't reach the orchestrator — starting from a suggested roster you can edit.");
      setRoles(proposeRoles().map((r) => ({ ...r, id: RID++ })));
      setExecutionPlan(null);
      setExecutionPlanDraft("");
      setMilestones([]);
    } finally {
      setComposing(false);
      setComposed(true);
    }
  }

  // Unified intake: when arriving with a prefilled charter (from the single
  // intent-first entry that already classified this as standing team work),
  // compose the org chart immediately — the user lands on the editable roster,
  // not an empty charter box. Profiles already arrive pre-composed.
  const autoRan = useRef(false);
  useEffect(() => {
    if (!autoRan.current && !profile && initialCharter && initialCharter.trim().length >= 8) {
      autoRan.current = true;
      void compose();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const addRole = () => setRoles((r) => [...r, { id: RID++, name: "", system_prompt: "", runtime: "", model: "", skills: [] }]);
  // Drop a reusable member archetype (e.g. Rust Engineer) into the roster,
  // pre-filled with its charge; runtime/model stay unset to inherit the team
  // default. Skills are applied only if this cluster actually has them.
  const addFromArchetype = (id: string) => {
    const a = MEMBER_ARCHETYPES.find((x) => x.id === id);
    if (!a) return;
    // B2: dedup — the archetype dropdown gives no confirmation and resets to
    // its initial prompt, so users spam-click it thinking nothing happened and
    // spray duplicate roles. Adding an archetype already in the roster is a
    // no-op (a role can still be added manually via "+ Add role" if a second
    // instance is genuinely wanted). Also flash a visible note so the add isn't
    // silent (audit B2).
    setRoles((r) => {
      if (r.some((x) => x.name === a.id)) {
        setAddNote(`“${a.title ?? a.id}” is already in the roster`);
        return r;
      }
      setAddNote(`Added “${a.title ?? a.id}” to the roster`);
      return [
        ...r,
        {
          id: RID++,
          name: a.id,
          system_prompt: a.system_prompt,
          runtime: "",
          model: "",
          skills: a.suggestedSkills.filter((s) => availableSkillNames.has(s)),
        },
      ];
    });
  };
  const removeRole = (id: number) => setRoles((r) => r.filter((x) => x.id !== id));
  const patchRole = (id: number, p: Partial<Role>) => setRoles((r) => r.map((x) => (x.id === id ? { ...x, ...p } : x)));
  const addMilestone = () =>
    setMilestones((current) => [
      ...current,
      {
        id: `milestone-${current.length + 1}`,
        title: "",
        description: "",
        owner_role: null,
        depends_on: current.length > 0 ? [current[current.length - 1].id] : [],
        acceptance_criteria: [],
        review_required: false,
      },
    ]);
  const patchMilestone = (index: number, patch: Partial<ComposeTeamMilestone>) =>
    setMilestones((current) =>
      current.map((milestone, currentIndex) =>
        currentIndex === index ? { ...milestone, ...patch } : milestone,
      ),
    );

  const rolesJson = useMemo(() => JSON.stringify(roles.map(({ name, system_prompt, runtime, model, skills }) => ({ name, system_prompt, runtime, model, skills }))), [roles]);
  const milestonesJson = useMemo(() => JSON.stringify(milestones), [milestones]);
  const executionPlanJson = useMemo(
    () => JSON.stringify(executionPlan),
    [executionPlan],
  );

  // Step 1 — intent.
  if (!composed) {
    return (
      <div className="space-y-5">
        {composing && (
          <OrchestrationCube
            title="Composing your team"
            done={false}
            active={0}
            phases={[
              { icon: "layers", label: "Reading the cluster palette", detail: `${options.models.length} model${options.models.length === 1 ? "" : "s"} · ${options.runtimes.filter((r) => r.wired).length} harness${options.runtimes.filter((r) => r.wired).length === 1 ? "" : "es"}` },
              { icon: "branch", label: "Proposing the org chart", detail: "principal + roles, each a bounded subset of the team envelope" },
              { icon: "gear", label: "Assigning per-role harness & model", detail: "informed by the efficiency frontier where available" },
              { icon: "note", label: "Preparing the org for review", detail: charter.trim().slice(0, 72) || "your charter" },
            ]}
          />
        )}
      <div className="grid gap-5 lg:grid-cols-[1.1fr_0.9fr]">
        <div className="kb-card kb-canvas p-6">
          <h2 className="text-sm font-semibold">What should this team do?</h2>
          <p className="mt-0.5 text-xs text-foreground-muted">A standing team runs continuously under a charter. Describe its mandate — kars proposes an org chart you can shape.</p>
          <label className="mt-4 block text-xs font-medium text-foreground-muted">Charter</label>
          <textarea value={charter} onChange={(e) => setCharter(e.target.value)} rows={5} autoFocus placeholder="e.g. Keep the kars repo healthy: triage new issues, watch open PRs, and report failing checks to the steering inbox." className="mt-1.5 w-full resize-y rounded-xl border border-border bg-surface px-4 py-3 text-sm leading-relaxed focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal" />
          {/* Loop engineering: the standing team runs its charter as a loop on
              every cadence tick; design it here so the loop + eval criteria
              reach the harness and every sub-agent the principal spawns. */}
          <div className="mt-3">
            <LoopDesigner surface="team" initialGoal={charter} onApply={setCharter} />
          </div>
          <div className="mt-5 flex justify-end">
            <button type="button" onClick={compose} disabled={charter.trim().length < 8 || composing} className="inline-flex items-center gap-2 rounded-lg bg-signal px-5 py-2.5 text-sm font-semibold text-signal-fg disabled:opacity-50">
              {composing ? (
                <>
                  <span className="relative flex h-2 w-2" aria-hidden>
                    <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-signal-fg/70" />
                    <span className="relative inline-flex h-2 w-2 rounded-full bg-signal-fg" />
                  </span>
                  Composing the org…
                </>
              ) : (
                "Compose the org →"
              )}
            </button>
          </div>
        </div>
        <div className="kb-card p-6">
          <h2 className="text-sm font-semibold">How a team is built</h2>
          <ul className="mt-4 space-y-2.5">
            {([["person", "Principal", "the team itself — holds the trust envelope and reporting line"], ["handshake", "Roles", "members that each do part of the charter"], ["gear", "Per-role harness & model", "different members can run different harnesses/models"], ["bolt", "Skills", "versioned capability bundles a role acquires"], ["loop", "Cadence", "how often the team wakes to act"]] as [IconName, string, string][]).map(([i, t, d]) => (
              <li key={t} className="flex items-start gap-2.5"><span className="mt-0.5"><Icon name={i} size={15} /></span><div><p className="text-sm font-medium">{t}</p><p className="text-xs text-foreground-muted">{d}</p></div></li>
            ))}
          </ul>
          <p className="mt-5 rounded-lg bg-surface-muted/60 px-3 py-2 text-[11px] text-foreground-muted">Each role&rsquo;s authority is a verified subset of the team&rsquo;s. Nothing runs until you create + launch.</p>
        </div>
      </div>
      </div>
    );
  }

  // Step 2 — the org chart, editable.
  return (
    <form action={action} className="space-y-5">
      <input type="hidden" name="charter" value={charter} />
      <input type="hidden" name="tier" value={tier} />
      <input type="hidden" name="cadence" value={cadence} />
      <input type="hidden" name="lifecycle_mode" value={lifecycleMode} />
      <input type="hidden" name="warm_idle_seconds" value={warmIdleMinutes * 60} />
      <input type="hidden" name="reporting_to" value={reporting} />
      <input type="hidden" name="display_name" value={displayName} />
      <input type="hidden" name="tool_policy" value={toolPolicy} />
      <input type="hidden" name="runtime" value={runtime} />
      <input type="hidden" name="model" value={model} />
      <input type="hidden" name="model_fallbacks_json" value={JSON.stringify(modelFallbacks)} />
      <input type="hidden" name="mcp_servers" value={mcp.join(",")} />
      <input type="hidden" name="memory" value={memory} />
      <input type="hidden" name="egress_mode" value={egressMode} />
      <input type="hidden" name="egress_json" value={JSON.stringify(parseEgressLines(egressText))} />
      <input type="hidden" name="execution_plan_json" value={executionPlanJson} />
      <input type="hidden" name="knowledge_commons" value={commons} />
      <input type="hidden" name="roles_json" value={rolesJson} />
      <input type="hidden" name="milestones_json" value={milestonesJson} />
      <input type="hidden" name="engineering_enabled" value={engineeringEnabled ? "true" : "false"} />
      <input type="hidden" name="engineering_signals" value={[...engineeringSignals].join(",")} />
      <input type="hidden" name="engineering_poll_interval_seconds" value={engineeringPoll} />
      <input type="hidden" name="engineering_auto_run" value={engineeringAutoRun ? "true" : "false"} />

      {rationale && (
        <div className="rounded-xl border border-signal/30 bg-signal/5 p-4">
          <p className="text-xs font-semibold text-foreground">Why this org</p>
          <p className="mt-1 text-sm text-foreground-muted">{rationale}</p>
          {modelBasis && (
            <p className="mt-2 text-xs text-foreground">
              <span className="font-medium">Model route:</span> {modelBasis}
            </p>
          )}
          <p className="mt-2 text-[11px] text-foreground-muted">
            {expectedTokens != null
              ? `Historical expectation: about ${expectedTokens.toLocaleString()} tokens per delivered outcome across ${efficiencyRuns} retained run${efficiencyRuns === 1 ? "" : "s"}. `
              : "No reliable cost expectation is available yet. "}
            Composed from your charter, the cluster&apos;s live capabilities, and retained efficiency
            evidence. Edit anything below.
          </p>
        </div>
      )}
      {composeNote && (
        <div className="rounded-xl border border-border bg-surface-muted/50 p-4 text-sm text-foreground-muted">
          {composeNote}
        </div>
      )}

      <div className="kb-card p-5 sm:p-6">
        <h2 className="text-sm font-semibold">Team basics</h2>
        <fieldset className="mt-3 rounded-lg border border-border p-3">
          <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="person" size={13} /> Identity</legend>
          <div className="grid gap-3 sm:grid-cols-2">
            <label className="text-xs text-foreground-muted">Name<input name="name" value={name} onChange={(e) => setName(e.target.value)} required placeholder="repo-watch" className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm" /></label>
            <label className="text-xs text-foreground-muted">Display name (optional)<input value={displayName} onChange={(e) => setDisplayName(e.target.value)} placeholder="Repo Watch" className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm" /></label>
          </div>
        </fieldset>
        <fieldset className="mt-3 rounded-lg border border-border p-3">
          <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="loop" size={13} /> Cadence & authority</legend>
          <div className="grid gap-3 sm:grid-cols-3">
            <label className="text-xs text-foreground-muted">Autonomy tier<select value={tier} onChange={(e) => setTier(Number(e.target.value))} className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm"><option value={1}>1 Manual</option><option value={2}>2 Shared</option><option value={3}>3 Conditional</option><option value={4}>4 Supervised</option><option value={5}>5 Full</option></select></label>
            <label className="text-xs text-foreground-muted">Cadence (min, 0 = passive)<input type="number" min={0} value={cadence} onChange={(e) => setCadence(Number(e.target.value))} className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm" /></label>
            <label className="text-xs text-foreground-muted">Reports to (optional)<input value={reporting} onChange={(e) => setReporting(e.target.value)} placeholder="steering inbox" className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm" /></label>
          </div>
        </fieldset>
        <fieldset className="mt-3 rounded-lg border border-border p-3">
          <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="box" size={13} /> Runtime lifecycle</legend>
          <div className="grid gap-3 sm:grid-cols-3">
            <label className="text-xs text-foreground-muted">
              Retention mode
              <select
                value={lifecycleMode}
                onChange={(event) => setLifecycleMode(event.target.value as TeamLifecycleMode)}
                className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm"
              >
                <option value="resourceOptimized">Resource optimized (recommended)</option>
                <option value="persistent">Persistent</option>
                <option value="ephemeral">Ephemeral</option>
              </select>
            </label>
            {lifecycleMode === "resourceOptimized" && (
              <label className="text-xs text-foreground-muted">
                Warm idle window (minutes)
                <input
                  type="number"
                  min={0}
                  value={warmIdleMinutes}
                  onChange={(event) => setWarmIdleMinutes(Number(event.target.value))}
                  className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm"
                />
              </label>
            )}
            <p className="self-end text-xs text-foreground-muted sm:col-span-1">
              {lifecycleMode === "persistent"
                ? "Keeps the principal ready until you pause the team."
                : lifecycleMode === "resourceOptimized"
                  ? "Reuses the same principal while warm, then suspends it without losing identity or memory."
                  : "Starts a clean isolated runtime for each assignment and tears it down after evidence is retained."}
            </p>
          </div>
        </fieldset>
        {/* Advanced governance — real team-level access controls the create API
            supports: the tool policy that bounds every run, and the shared
            knowledge commons its runs read/write. Defaults are safe when unset. */}
        {renderGovernancePanel({
          name,
          options,
          mcp,
          setMcp,
          toolPolicy,
          setToolPolicy,
          commons,
          setCommons,
          memory,
          setMemory,
          selectedMemoryOption,
          runtime,
          setRuntime,
          model,
          setModel,
          modelFallbacks,
          setModelFallbacks,
          egressMode,
          setEgressMode,
          egressText,
          setEgressText,
        })}
        <p className="mt-3 text-xs text-foreground-muted">Charter: <span className="text-foreground">{charter}</span></p>
      </div>

      {/* The org chart. */}
      {renderOrgPanel({
        name,
        tier,
        roles,
        options,
        addFromArchetype,
        addRole,
        addNote,
        patchRole,
        removeRole,
      })}

      <div className="kb-card p-5 sm:p-6">
        <h2 className="text-sm font-semibold">Typed execution plan</h2>
        <p className="mt-0.5 text-xs text-foreground-muted">
          Explicit dependencies, phases, capabilities, tool-call bounds, and synthesis. No permissions are inferred from role names.
        </p>
        {executionPlanDraft ? (
          <>
            <textarea
              value={executionPlanDraft}
              onChange={(event) => {
                const next = event.target.value;
                setExecutionPlanDraft(next);
                try {
                  const parsed = JSON.parse(next) as ExecutionPlan;
                  setExecutionPlan(parsed);
                  setExecutionPlanError(null);
                } catch {
                  setExecutionPlanError("The execution plan must be valid JSON.");
                }
              }}
              rows={18}
              spellCheck={false}
              className="mt-3 w-full resize-y rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs leading-relaxed focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
            />
            {executionPlanError && (
              <p className="mt-2 text-xs text-danger">{executionPlanError}</p>
            )}
          </>
        ) : (
          <p className="mt-3 rounded-lg border border-danger/30 bg-danger/5 px-3 py-2 text-xs text-danger">
            No execution plan is available. Re-run composition before creating the team.
          </p>
        )}
      </div>

      <div className="kb-card p-5 sm:p-6">
        <div className="flex items-start justify-between gap-3">
          <div>
            <h2 className="text-sm font-semibold">Milestone graph</h2>
            <p className="mt-0.5 text-xs text-foreground-muted">
              Durable, resumable work packets. A milestone runs only after every dependency is done;
              acceptance criteria and artifact ownership travel with the assignment.
            </p>
          </div>
          <button
            type="button"
            onClick={addMilestone}
            className="rounded-lg border border-border px-3 py-1.5 text-xs font-medium hover:bg-surface-muted"
          >
            + Add milestone
          </button>
        </div>
        <div className="mt-4 space-y-3">
          {milestones.map((milestone, index) => (
            <div key={`${milestone.id}-${index}`} className="rounded-xl border border-border bg-surface p-3">
              <div className="grid gap-2 sm:grid-cols-[0.8fr_1.5fr_auto]">
                <input
                  value={milestone.id}
                  onChange={(event) => patchMilestone(index, { id: event.target.value })}
                  placeholder="stable-id"
                  className="rounded-lg border border-border bg-surface px-2.5 py-1.5 font-mono text-xs"
                />
                <input
                  value={milestone.title}
                  onChange={(event) => patchMilestone(index, { title: event.target.value })}
                  placeholder="Milestone title"
                  className="rounded-lg border border-border bg-surface px-2.5 py-1.5 text-sm font-medium"
                />
                <button
                  type="button"
                  onClick={() => setMilestones((current) => current.filter((_, i) => i !== index))}
                  className="text-xs text-foreground-muted hover:text-danger"
                >
                  Remove
                </button>
              </div>
              <textarea
                value={milestone.description}
                onChange={(event) => patchMilestone(index, { description: event.target.value })}
                rows={2}
                placeholder="Work, expected artifact, and handoff boundary"
                className="mt-2 w-full rounded-lg border border-border bg-surface px-2.5 py-1.5 text-xs"
              />
              <div className="mt-2 grid gap-2 sm:grid-cols-3">
                <label className="text-[11px] text-foreground-muted">
                  Owner role
                  <select
                    value={milestone.owner_role ?? ""}
                    onChange={(event) => patchMilestone(index, { owner_role: event.target.value || null })}
                    className="mt-1 w-full rounded-lg border border-border bg-surface px-2.5 py-1.5 text-xs"
                  >
                    <option value="">Principal / assign dynamically</option>
                    {roles.filter((role) => role.name.trim()).map((role) => (
                      <option key={role.id} value={role.name}>{role.name}</option>
                    ))}
                  </select>
                </label>
                <label className="text-[11px] text-foreground-muted">
                  Depends on
                  <input
                    value={milestone.depends_on.join(", ")}
                    onChange={(event) =>
                      patchMilestone(index, {
                        depends_on: event.target.value.split(",").map((value) => value.trim()).filter(Boolean),
                      })
                    }
                    placeholder="earlier-id"
                    className="mt-1 w-full rounded-lg border border-border bg-surface px-2.5 py-1.5 font-mono text-xs"
                  />
                </label>
                <label className="text-[11px] text-foreground-muted">
                  Acceptance criteria
                  <textarea
                    value={milestone.acceptance_criteria.join("\n")}
                    onChange={(event) =>
                      patchMilestone(index, {
                        acceptance_criteria: event.target.value.split("\n").map((value) => value.trim()).filter(Boolean),
                      })
                    }
                    rows={2}
                    placeholder={"Tests pass\nArtifact is reviewable"}
                    className="mt-1 w-full rounded-lg border border-border bg-surface px-2.5 py-1.5 text-xs"
                  />
                </label>
              </div>
              <label className="mt-2 flex items-center gap-2 text-[11px] font-medium text-foreground-muted">
                <input
                  type="checkbox"
                  checked={milestone.review_required}
                  onChange={(event) => patchMilestone(index, { review_required: event.target.checked })}
                  className="h-3.5 w-3.5 rounded border-border accent-signal"
                />
                Pause after delivery for customer review before dependent milestones unlock
              </label>
            </div>
          ))}
          {milestones.length === 0 && (
            <p className="rounded-lg border border-dashed border-border px-3 py-4 text-center text-xs text-foreground-muted">
              No finite milestone graph — appropriate for continuous monitoring. Add milestones for
              builds, launches, migrations, research programs, and campaigns.
            </p>
          )}
        </div>
      </div>

      <PreflightCheck
        key={teamFingerprint}
        blueprint={teamBlueprint}
        tier={tier}
        workload="team"
        onResult={(result) => {
          setValidation(result);
          setValidatedFingerprint(teamFingerprint);
        }}
      />

      <fieldset className="rounded-xl border border-border p-4">
        <legend className="px-1 text-xs font-medium text-foreground-muted">
          Continuous engineering intake
        </legend>
        <label className="flex items-center gap-2 text-sm font-medium">
          <input
            type="checkbox"
            checked={engineeringEnabled}
            onChange={(event) => setEngineeringEnabled(event.target.checked)}
            className="h-4 w-4 rounded border-border accent-signal"
          />
          Monitor the selected repositories and queue new work
        </label>
        {engineeringEnabled && (
          <div className="mt-3 space-y-3">
            <div className="grid gap-2 sm:grid-cols-2">
              {[
                ["dependabot_pr", "Dependabot pull requests"],
                ["dependabot_alert", "Dependabot vulnerability alerts"],
                ["code_scanning_alert", "Code scanning / code-quality alerts"],
                ["secret_scanning_alert", "Secret scanning alerts"],
              ].map(([signal, label]) => (
                <label key={signal} className="flex items-center gap-2 text-xs">
                  <input
                    type="checkbox"
                    checked={engineeringSignals.has(signal)}
                    onChange={(event) =>
                      setEngineeringSignals((current) => {
                        const next = new Set(current);
                        if (event.target.checked) next.add(signal);
                        else next.delete(signal);
                        return next;
                      })
                    }
                    className="h-3.5 w-3.5 rounded border-border accent-signal"
                  />
                  {label}
                </label>
              ))}
            </div>
            <div className="grid gap-3 sm:grid-cols-2">
              <label className="text-xs text-foreground-muted">
                Poll interval (seconds)
                <input
                  type="number"
                  min={300}
                  max={86400}
                  value={engineeringPoll}
                  onChange={(event) => setEngineeringPoll(Number(event.target.value))}
                  className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm"
                />
              </label>
              <label className="flex items-center gap-2 self-end pb-2 text-xs font-medium">
                <input
                  type="checkbox"
                  checked={engineeringAutoRun}
                  onChange={(event) => setEngineeringAutoRun(event.target.checked)}
                  className="h-3.5 w-3.5 rounded border-border accent-signal"
                />
                Start the team when new work is queued
              </label>
            </div>
            <p className="text-[11px] text-foreground-muted">
              Repository access below is used both for intake and keyless PR delivery. GitHub
              security signals remain honest Partial/Unavailable when permissions or products are
              not enabled.
            </p>
          </div>
        )}
      </fieldset>

      <RepoAccess onSelectionChange={setSelectedRepos} />

      <div className="flex flex-wrap items-center gap-3">
        <button type="submit" disabled={pending || !name.trim() || executionPlan === null || executionPlanError !== null || (engineeringEnabled && (selectedRepos.length === 0 || engineeringSignals.size === 0)) || (currentValidation !== null && !currentValidation.ok) || (launch && currentValidation?.ok !== true)} className="rounded-lg bg-signal px-5 py-2.5 text-sm font-semibold text-signal-fg disabled:opacity-50">{pending ? "Creating…" : "Create team"}</button>
        <label className="inline-flex items-center gap-2 text-xs text-foreground-muted">
          <input type="checkbox" name="launch" checked={launch} onChange={(event) => setLaunch(event.target.checked)} className="h-3.5 w-3.5 rounded border-border" />
          Launch immediately (otherwise created paused for your approval)
        </label>
        <button type="button" onClick={() => setComposed(false)} className="text-sm text-foreground-muted hover:text-foreground">← Back to charter</button>
        {!name.trim() && <p className="text-xs text-danger">Give the team a name to create it.</p>}
        {currentValidation !== null && !currentValidation.ok && <p className="text-xs text-danger">Fix the failing pre-flight checks before creating.</p>}
        {launch && currentValidation === null && <p className="text-xs text-danger">Validate the current package before launching immediately.</p>}
        {state.error && <p className="text-xs text-danger">{state.error}</p>}
      </div>
    </form>
  );
}
