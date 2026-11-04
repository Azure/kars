// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

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
  Options,
  ProfileSummary,
  ValidationResult,
} from "@/lib/types";
import { PreflightCheck } from "@/components/preflight-check";
import { CompositionNote } from "@/components/validation-checks";
import { RoleBudgetFields } from "@/components/role-budget-fields";
import { compositionSpending, roleBudgetError } from "@/lib/budget-review";
import { OrchestrationCube } from "@/components/orchestration-cube";
import { LoopDesigner } from "@/components/loop-designer";
import { Icon, type IconName } from "@/components/icon";
import { MEMBER_ARCHETYPES } from "@/lib/member-archetypes";
import { parseTeamBudget } from "@/lib/team-budget";
import { readTeamProfile } from "@/lib/team-profile";
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

export function TeamComposer({ options, profile, profileEnvelope, initialCharter, canQualify = false }: {
  options: Options;
  canQualify?: boolean;
  profile?: ProfileSummary | null;
  profileEnvelope?: import("@/lib/team-profile").TeamProfileEnvelope;
  initialCharter?: string;
}) {
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
  const [selectedProfile, setSelectedProfile] = useState(profile ?? null);
  const [name, setName] = useState("");
  const [displayName, setDisplayName] = useState(profile?.display_name ?? "");
  const [charter, setCharter] = useState(profile?.charter_template ?? initialCharter ?? "");
  const [tier, setTier] = useState(profileEnvelope?.tier ?? profile?.tier ?? 3);
  const [authorityCeiling, setAuthorityCeiling] = useState(profileEnvelope?.authorityCeiling.toString() ?? "");
  const [delegationDepth, setDelegationDepth] = useState(profileEnvelope?.delegationDepth.toString() ?? "");
  const [budgetTokens, setBudgetTokens] = useState(profileEnvelope?.budget?.tokens?.toString() ?? "");
  const [budgetUsdMicros, setBudgetUsdMicros] = useState(profileEnvelope?.budget?.usd_micros?.toString() ?? "");
  const budgetScope = "GovernedInference";
  const parsedBudget = parseTeamBudget(budgetTokens, budgetScope, budgetUsdMicros);
  const [addNote, setAddNote] = useState<string | null>(null);
  useEffect(() => { if (!addNote) return; const t = setTimeout(() => setAddNote(null), 2500); return () => clearTimeout(t); }, [addNote]);
  const [cadence, setCadence] = useState(0);
  const [reporting, setReporting] = useState("");
  const [toolPolicy, setToolPolicy] = useState(profileEnvelope?.toolPolicy ?? profile?.tool_policy ?? "");
  const [pinToolPolicy, setPinToolPolicy] = useState(!!profileEnvelope?.toolPolicy);
  const [runtime, setRuntime] = useState("");
  const [model, setModel] = useState("");
  const [modelFallbacks, setModelFallbacks] = useState<string[]>([]);
  const [mcp, setMcp] = useState<string[]>([]);
  const [memory, setMemory] = useState("");
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
        skills: r.skills ?? [],
        resources: { mcp_servers: [], egress: [] },
      })) ?? [],
  );
  const [milestones, setMilestones] = useState<ComposeTeamMilestone[]>([]);
  const [executionPlan, setExecutionPlan] = useState<object | null>(null);
  const [executionPlanDraft, setExecutionPlanDraft] = useState("");
  const [executionPlanError, setExecutionPlanError] = useState<string | null>(null);
  const [validation, setValidation] = useState<ValidationResult | null>(null);
  const [validatedFingerprint, setValidatedFingerprint] = useState<string | null>(null);
  const [launch, setLaunch] = useState(false);
  const [qualificationRun, setQualificationRun] = useState(false);
  // The team's effective package for the shared pre-flight — the same blueprint
  // shape a mission validates. The controller defaults toolPolicy to kars-default
  // and the model to the controller default, so we validate those effective
  // values. Roster-level model overrides ride on top per member.
  const teamBlueprint = useMemo(() => {
    const effectiveModel = model.includes("::") ? model : undefined;
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
      egress_mode: "strict" as const,
      execution_plan: executionPlan,
    };
  }, [runtime, toolPolicy, roles, mcp, memory, model, modelFallbacks, egressText, executionPlan]);
  const teamFingerprint = useMemo(
    () => JSON.stringify({ blueprint: teamBlueprint, roles, tier, authorityCeiling, delegationDepth, budgetTokens, budgetUsdMicros, budgetScope, pinToolPolicy, qualificationRun, selectedRepos }),
    [teamBlueprint, roles, tier, authorityCeiling, delegationDepth, budgetTokens, budgetUsdMicros, budgetScope, pinToolPolicy, qualificationRun, selectedRepos],
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
    const requested = compositionSpending(budgetTokens, executionPlanDraft);
    if (requested.error !== null || parsedBudget.error !== null) {
      setComposeNote(requested.error ?? parsedBudget.error);
      setComposed(true);
      return;
    }
    setComposing(true);
    setLaunch(false);
    setValidation(null);
    setValidatedFingerprint(null);
    setRationale(null);
    setModelBasis(null);
    setExpectedTokens(null);
    setEfficiencyRuns(0);
    setComposeNote(null);
    try {
      const res = await composeTeamAction(charter.trim(), selectedProfile?.name, qualificationRun || undefined, requested.spending);
      if (res.available && res.proposal) {
        // AI orchestrator composed the org — adopt its roster + team settings.
        const p = res.proposal;
        if (selectedProfile && p.profile?.name !== selectedProfile.name) {
          setComposeNote("The selected profile was omitted or replaced. The existing draft was kept; nothing was launched.");
          return;
        }
        const copied = p.profile ? readTeamProfile(p.profile, options.skills, options.tool_policies) : null;
        if (copied?.error) {
          setComposeNote(copied.error);
          return;
        }
        const missingSkills = [...new Set(p.roles.flatMap((role) => role.skills ?? []))]
          .filter((skill) => !availableSkillNames.has(skill));
        if (missingSkills.length > 0) {
          setComposeNote(`Proposed skills are unavailable: ${missingSkills.join(", ")}. The existing draft was kept; no skills were silently removed.`);
          return;
        }
        if (p.profile && copied?.envelope) {
          const envelope = copied.envelope;
          const missingRoles = p.profile.roles.some((role) => !p.roles.some((member) =>
            member.name === role.name && (role.skills ?? []).every((skill) => member.skills.includes(skill))));
          if (missingRoles || p.tier !== envelope.tier || (envelope.budget?.tokens != null &&
            (p.budget_tokens == null || p.budget_tokens > envelope.budget.tokens))) {
            setComposeNote("The proposal does not preserve the profile roles, skills, tier or lifetime budget. The existing draft was kept; nothing was launched.");
            return;
          }
        }
        setBudgetTokens(p.budget_tokens?.toString() ?? "");
        if (!selectedProfile) {
          setTier(p.tier);
          if (p.profile && copied?.envelope) {
            const envelope = copied.envelope;
            setSelectedProfile(p.profile);
            setDisplayName(p.profile.display_name ?? "");
            setAuthorityCeiling(envelope.authorityCeiling.toString());
            setDelegationDepth(envelope.delegationDepth.toString());
            const profileCost = envelope.budget?.usd_micros;
            const requestedCost = parsedBudget.budget?.usd_micros;
            setBudgetUsdMicros((profileCost != null && requestedCost != null
              ? Math.min(profileCost, requestedCost) : requestedCost ?? profileCost)?.toString() ?? "");
            setToolPolicy(envelope.toolPolicy ?? p.profile.tool_policy ?? "");
            setPinToolPolicy(!!envelope.toolPolicy);
            setCommons(p.profile.knowledge_commons ?? "");
          }
        }
        setCadence(p.cadence_minutes);
        setModel(p.model ?? "");
        setModelFallbacks(p.model_fallbacks ?? []);
        setMcp((p.mcp_servers ?? []).filter((server) => availableMcpNames.has(server)));
        setMemory(p.memory && availableMemoryNames.has(p.memory) ? p.memory : "");
        setEgressText(
          (p.egress ?? [])
            .map((entry) => `${entry.host}${entry.port ? `:${entry.port}` : ""}`)
            .join("\n"),
        );
        setEngineeringEnabled(!qualificationRun && p.engineering_enabled);
        setEngineeringSignals(new Set(p.engineering_signals ?? []));
        setEngineeringPoll(p.engineering_poll_interval_seconds ?? 900);
        setEngineeringAutoRun(p.engineering_auto_run ?? true);
        setExecutionPlan(p.execution_plan);
        setExecutionPlanDraft(p.execution_plan ? JSON.stringify(p.execution_plan, null, 2) : "");
        setExecutionPlanError(null);
        const proposedRoles = p.roles.filter((role) => role.name.trim().toLowerCase() !== "principal");
        setRoles(
          (proposedRoles.length ? proposedRoles : proposeRoles()).map((r) => ({
            id: RID++,
            name: r.name,
            system_prompt: r.system_prompt,
            runtime: "runtime" in r ? r.runtime : "",
            model: "model" in r ? r.model : "",
            skills: r.skills ?? [],
            resources: r.resources ?? { mcp_servers: [], egress: [] },
          })),
        );
        setMilestones(p.milestones ?? []);
        setRationale(res.rationale ?? null);
        setModelBasis(p.model_basis ?? null);
        setExpectedTokens(p.expected_tokens_per_outcome ?? null);
        setEfficiencyRuns(p.efficiency_sample_runs ?? 0);
      } else {
        setComposeNote(res.reason ?? null);
        if (selectedProfile || executionPlanDraft.trim() || composed) return;
        setRoles(proposeRoles().map((r) => ({ ...r, id: RID++ })));
        setExecutionPlan(null);
        setExecutionPlanDraft("");
        setMilestones([]);
        if (/dependabot|code quality|code scanning|security finding|repository maintenance/i.test(charter)) {
          setEngineeringEnabled(!qualificationRun);
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
      setComposeNote("Couldn't reach the orchestrator. Nothing was launched; the draft remains editable.");
      if (selectedProfile || executionPlanDraft.trim() || composed) return;
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
  const addRole = () => setRoles((r) => [...r, { id: RID++, name: "", system_prompt: "", runtime: "", model: "", skills: [], resources: { mcp_servers: [], egress: [] } }]);
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
          resources: { mcp_servers: [], egress: [] },
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

  const qualificationChoice = canQualify ? (
    <label className="block rounded-lg border border-border p-3 text-xs text-foreground-muted">
      <input type="checkbox" checked={qualificationRun} disabled={composing || pending} onChange={(event) => {
        setQualificationRun(event.target.checked);
        if (event.target.checked && !runtime) setRuntime("OpenClaw");
        if (event.target.checked) setEngineeringEnabled(false);
        setLaunch(false);
      }} className="mr-2 accent-signal" />
      Operator qualification: one reviewed Team attempt, then pause. Missing prior route/resource evidence is advisory, not qualified evidence. Policies, credentials, isolation, content-bound skill approval and finite budgets still apply. Approval expires after 45 minutes.
    </label>
  ) : null;

  const rolesJson = useMemo(() => JSON.stringify(roles.map(({ name, system_prompt, runtime, model, skills, resources }) => ({ name, system_prompt, runtime, model, skills, resources }))), [roles]);
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
          {qualificationChoice}
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

  const planError = executionPlanError ?? roleBudgetError(executionPlan);
  function changeExecutionPlan(next: string) {
    setExecutionPlanDraft(next);
    try {
      const parsed: unknown = JSON.parse(next);
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
        setExecutionPlan(null);
        setExecutionPlanError("The execution plan must be a JSON object. The server validates its roles, phases and bounds.");
        return;
      }
      setExecutionPlan(parsed);
      setExecutionPlanError(roleBudgetError(parsed));
    } catch {
      setExecutionPlan(null);
      setExecutionPlanError("The execution plan must be valid JSON.");
    }
  }

  // Step 2 — the org chart, editable.
  return (
    <form action={action} className="space-y-5">
      {qualificationChoice}
      <input type="hidden" name="qualification_run" value={qualificationRun ? "true" : "false"} />
      <input type="hidden" name="charter" value={charter} />
      <input type="hidden" name="tier" value={tier} />
      <input type="hidden" name="cadence" value={cadence} />
      <input type="hidden" name="reporting_to" value={reporting} />
      <input type="hidden" name="display_name" value={displayName} />
      <input type="hidden" name="tool_policy" value={toolPolicy} />
      <input type="hidden" name="runtime" value={runtime} />
      <input type="hidden" name="model" value={model} />
      <input type="hidden" name="model_fallbacks_json" value={JSON.stringify(modelFallbacks)} />
      <input type="hidden" name="mcp_servers" value={mcp.join(",")} />
      <input type="hidden" name="memory" value={memory} />
      <input type="hidden" name="egress_mode" value="strict" />
      <input type="hidden" name="egress_json" value={JSON.stringify(parseEgressLines(egressText))} />
      <input type="hidden" name="execution_plan_json" value={executionPlanJson} />
      <input type="hidden" name="knowledge_commons" value={commons} />
      <input type="hidden" name="roles_json" value={rolesJson} />
      <input type="hidden" name="milestones_json" value={milestonesJson} />
      <input type="hidden" name="engineering_enabled" value={engineeringEnabled ? "true" : "false"} />
      <input type="hidden" name="engineering_signals" value={[...engineeringSignals].join(",")} />
      <input type="hidden" name="engineering_poll_interval_seconds" value={engineeringPoll} />
      <input type="hidden" name="engineering_auto_run" value={engineeringAutoRun ? "true" : "false"} />

      {selectedProfile && (
        <div className="rounded-xl border border-border bg-surface-muted/50 p-4 text-sm">
          <p className="font-semibold">Team profile proposed for review: {selectedProfile.display_name ?? selectedProfile.name}</p>
          <p className="mt-1 text-xs text-foreground-muted">
            {selectedProfile.name} · {selectedProfile.domain ?? "team"} · {selectedProfile.phase}
            {selectedProfile.template_digest ? ` · Template ${selectedProfile.template_digest}` : ""}
          </p>
          <p className="mt-2 text-xs text-foreground-muted">Copied template defaults and bounds are shown below. Your charter remains the task. This is an editable snapshot, not profile authority or approval. Ready does not qualify MCP, skills or routes; preflight and explicit launch still apply.</p>
        </div>
      )}
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
            {profile
              ? "Based on the selected profile. Later profile edits do not update this draft. Review the proposed settings below; a template is not launch approval."
              : "Composed from your charter, the cluster's live capabilities, and retained efficiency evidence. Edit anything below."}
          </p>
        </div>
      )}
      {composeNote && (
        <div className="rounded-xl border border-border bg-surface-muted/50 p-4 text-sm text-foreground-muted">
          <CompositionNote detail={composeNote} />
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
            <label className="text-xs text-foreground-muted">Authority ceiling<input name="authority_ceiling" type="number" min={1} max={tier} step={1} value={authorityCeiling} onChange={(e) => setAuthorityCeiling(e.target.value)} placeholder="Same as tier" className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm" /></label>
            <label className="text-xs text-foreground-muted">Delegation depth<input name="delegation_depth" type="number" min={0} max={2147483647} step={1} value={delegationDepth} onChange={(e) => setDelegationDepth(e.target.value)} placeholder="Server reviewed-workflow default" className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm" /></label>
          </div>
          <p className="mt-2 text-xs text-foreground-muted">Explicit limits are preserved, not increased automatically. Reviewed execution may require more delegation hops; the server checks this before launch.</p>
        </fieldset>
        <fieldset className="mt-3 rounded-lg border border-border p-3">
          <legend className="flex items-center gap-1.5 px-1 text-xs font-medium text-foreground-muted"><Icon name="box" size={13} /> Runtime lifecycle</legend>
          <p className="text-xs text-foreground-muted">
            Durable Team, on-demand agent pods. The charter, history, shared memory and lifetime budget persist
            across isolated runs. Warm and always-on agent runtimes are not currently supported.
          </p>
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
          egressText,
          setEgressText,
        })}
        <label className="mt-3 flex items-start gap-2 text-xs text-foreground-muted">
          <input name="pin_tool_policy" type="checkbox" checked={pinToolPolicy} onChange={(event) => setPinToolPolicy(event.target.checked)} />
          Pin the selected tool policy in the authority envelope. Descendants must retain this exact policy reference.
        </label>
        <p className="mt-3 text-xs text-foreground-muted">Charter: <span className="text-foreground">{charter}</span></p>
      </div>

      {/* The org chart. */}
      {renderOrgPanel({
        resources: { mcp_servers: mcp, egress: teamBlueprint.egress, memory: memory || null },
        name,
        tier,
        authorityCeiling,
        delegationDepth,
        roles,
        options,
        addFromArchetype,
        addRole,
        addNote,
        patchRole,
        removeRole,
      })}

      <div className="kb-card p-5 sm:p-6">
        <h2 className="text-sm font-semibold">Work plan</h2>
        <p className="mt-0.5 text-xs text-foreground-muted">
          The steps each member will take, the tools they may use, and how their results come together.
          Kars checks these permissions before work can start.
        </p>
        {!executionPlanDraft.trim() && (
          <div className="mt-3 rounded-lg border border-border bg-surface-muted/50 p-3 text-sm">
            <p>This draft needs a work plan. You do not need to write technical JSON.</p>
            <p className="mt-1 text-xs text-foreground-muted">
              Kars can use your charter and selected template to propose a plan and update this draft for review.
              Preparation uses AI and records usage. It does not create or launch a team, and does not increase your authority limits.
            </p>
            <button type="button" onClick={compose} disabled={composing || pending || charter.trim().length < 8} className="mt-3 rounded-lg bg-signal px-4 py-2 text-sm font-semibold text-signal-fg disabled:opacity-50">
              {composing ? "Preparing work plan…" : "Prepare work plan"}
            </button>
          </div>
        )}
        {executionPlanError && (
          <p role="alert" className="mt-2 text-xs text-danger">{executionPlanError}</p>
        )}
        <details className="mt-3">
          <summary className="cursor-pointer text-xs font-medium text-foreground-muted">Advanced: inspect or edit plan JSON</summary>
          <p className="mt-2 text-xs text-foreground-muted">
            Explicit dependencies, phases, capabilities, tool-call bounds, and synthesis. Role names must match the roster.
            No permissions are inferred from role names; JSON syntax alone does not authorize execution.
          </p>
          <textarea
            aria-label="Typed execution plan JSON"
            value={executionPlanDraft}
            onChange={(event) => changeExecutionPlan(event.target.value)}
            rows={18}
            spellCheck={false}
            className="mt-3 w-full resize-y rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs leading-relaxed focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
          />
        </details>
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

      <fieldset className="rounded-xl border border-border p-4">
        <legend className="px-1 text-sm font-semibold">Spending limits</legend>
        <p className="mb-4 max-w-prose text-sm leading-relaxed text-foreground-muted">
          Existing workspace and profile rules apply automatically. Add a limit only if you want
          a tighter cap for this Team across all its work, including retries and future runs.
          Tokens measure text the AI reads and writes; they are not dollars.
        </p>
        <input type="hidden" name="budget_scope" value={budgetScope} />
        <label htmlFor="team-budget-tokens" className="text-sm font-medium">Total token limit (optional)</label>
        <input
          id="team-budget-tokens"
          name="budget_tokens"
          type="number"
          min={1}
          max={Number.MAX_SAFE_INTEGER}
          step={1}
          placeholder="Use existing controls"
          value={budgetTokens}
          onChange={(event) => setBudgetTokens(event.target.value)}
          aria-describedby="team-budget-help team-budget-error"
          aria-invalid={parsedBudget.error !== null}
          className="mt-2 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm"
        />
        <p id="team-budget-help" className="mt-2 max-w-prose text-sm leading-relaxed text-foreground-muted">
          Blank means no shared token limit is requested here. Role, profile and workspace limits
          still apply, and usage is always recorded. This is not a new allowance for each run or
          a limit on tools and infrastructure costs. Confirm any limit here, even if you mentioned
          it in your request. Estimates do not change these settings.
        </p>
        <RoleBudgetFields draft={executionPlanDraft} onChange={changeExecutionPlan} idPrefix="team-role-budget" />
        <details className="mt-4 border-t border-border pt-3">
          <summary className="cursor-pointer text-sm font-medium">Optional cost limit (advanced)</summary>
        <label htmlFor="team-budget-usd-micros" className="mt-3 block text-sm font-medium">Lifetime inference cost limit (micro-USD, optional)</label>
        <input id="team-budget-usd-micros" name="budget_usd_micros" type="number" min={1} max={Number.MAX_SAFE_INTEGER} step={1}
          value={budgetUsdMicros} onChange={(event) => setBudgetUsdMicros(event.target.value)}
          aria-describedby="team-cost-help team-budget-error"
          className="mt-2 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm" />
        <p id="team-cost-help" className="mt-2 text-xs text-foreground-muted">1,000,000 micro-USD = $1. When set, both token and cost limits apply; the cost limit also requires an operator pricing contract. Blank means no separate cost limit.</p>
        </details>
        <p id="team-budget-error" role="alert" className="mt-2 text-xs text-danger">{parsedBudget.error}</p>
        {planError && <p role="alert" className="mt-2 text-xs text-danger">{planError}</p>}
      </fieldset>

      <PreflightCheck
        key={teamFingerprint}
        blueprint={teamBlueprint}
        selectedRepos={selectedRepos}
        teamRoles={roles.map(({ name, system_prompt, runtime, model, skills, resources }) => ({ name, system_prompt, runtime, model, skills, resources }))}
        tier={tier}
        delegationDepth={delegationDepth.trim() === "" ? undefined : Number(delegationDepth)}
        budgetTokens={parsedBudget.budget?.tokens}
        budgetUsdMicros={parsedBudget.budget?.usd_micros}
        budgetScope={budgetScope}
        disabled={parsedBudget.error !== null || executionPlan === null || planError !== null}
        onReviewBudget={() => document.getElementById("team-budget-tokens")?.focus()}
        workload="team"
        qualificationRun={qualificationRun}
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
            disabled={qualificationRun}
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

      <RepoAccess plan={executionPlan} selected={selectedRepos} onSelectionChange={(repos) => {
        setSelectedRepos(repos);
        setValidation(null);
        setValidatedFingerprint(null);
        setLaunch(false);
      }} />
      {selectedRepos.length > 0 && currentValidation?.ok !== true && (
        <p className="text-xs text-danger">Check readiness for these repositories before creating the team. Nothing has started yet.</p>
      )}

      <div className="flex flex-wrap items-center gap-3">
        <button type="submit" disabled={pending || parsedBudget.error !== null || !name.trim() || executionPlan === null || planError !== null || (engineeringEnabled && (selectedRepos.length === 0 || engineeringSignals.size === 0)) || (currentValidation !== null && !currentValidation.ok) || ((launch || selectedRepos.length > 0) && currentValidation?.ok !== true)} className="rounded-lg bg-signal px-5 py-2.5 text-sm font-semibold text-signal-fg disabled:opacity-50">{pending ? "Creating…" : "Create team"}</button>
        <label className="inline-flex items-center gap-2 text-xs text-foreground-muted">
          <input type="checkbox" name="launch" checked={launch} onChange={(event) => setLaunch(event.target.checked)} className="h-3.5 w-3.5 rounded border-border" />
          {qualificationRun ? "Approve one attempt of the first reviewed milestone, then pause" : "Launch immediately (otherwise created paused for your approval)"}
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
