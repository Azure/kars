// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { SegmentedTier } from "@/components/segmented-tier";
import { OrchestrationCube } from "@/components/orchestration-cube";
import { Icon } from "@/components/icon";
import { JourneyRail } from "@/components/journey-rail";
import { humanizeMcp } from "@/lib/format";
import { RepoAccess, missionGithubWriteEnabled } from "@/components/repo-access";
import { EnvelopeReveal } from "../envelope-reveal";
import { CreateButton, EgressEditor, PackageSection } from "./controls";
import { CompositionNote, ValidationChecks } from "@/components/validation-checks";
import { RoleBudgetFields } from "@/components/role-budget-fields";
import { roleBudgetError } from "@/lib/budget-review";
import { modelKey, moveFallback } from "./helpers";
import type { ReviewProps } from "./review-types";
import { QualificationChoice } from "./qualification-choice";

const TIER_CONSEQUENCE: Record<number, string> = {
  1: "Manual — the mission proposes every step and does nothing on its own. You perform each action.",
  2: "Shared — the mission acts only on low-risk steps; everything else waits for your approval.",
  3: "Conditional — the mission acts on its own but pauses for your approval before anything that costs money, touches external systems, or can't be undone.",
  4: "Supervised — the mission runs autonomously with periodic checkpoints you sign off on.",
  5: "Full — the mission runs autonomously within its budget and time limit; you review the result.",
};

export function renderReview({
  canQualify,
  qualificationRun,
  changeQualification,
  formAction,
  objective,
  tier,
  budgetTokens,
  launch,
  blueprint,
  selectedRepos = [],
  changeSelectedRepos,
  delegation,
  setDelegation,
  rationale,
  proposed,
  composeSource,
  recommendedRoute,
  modelBasis,
  composeNote,
  setObjective,
  options,
  model,
  setModel,
  setModelFallbacks,
  modelFallbacks,
  recommendedModel,
  efficiency,
  recommendedStats,
  runtime,
  changeRuntime,
  instructions,
  setInstructions,
  executionPlanDraft,
  setExecutionPlanDraft,
  setExecutionPlan,
  setExecutionPlanError,
  executionPlanError,
  loopDirective,
  cameViaLoopReview,
  goBackToLoopReview,
  toolPolicy,
  setToolPolicy,
  mcp,
  setMcp,
  mcpNeedsPolicy,
  skills,
  setSkills,
  egress,
  setEgress,
  isolation,
  setIsolation,
  memory,
  setMemory,
  setTier,
  ceiling,
  setBudgetTokens,
  runValidation,
  validating,
  validationError,
  validation,
  validationFresh,
  setLaunch,
  state,
  goBack,
}: ReviewProps) {
  function changeExecutionPlan(next: string) {
    setExecutionPlanDraft(next);
    if (next.trim() === "") {
      setDelegation({ mode: "single-agent", roles: [], max_parallel: 1 });
      setExecutionPlan(null);
      setExecutionPlanError(null);
      return;
    }
    try {
      const parsed = JSON.parse(next) as import("@/lib/types").ExecutionPlan;
      setExecutionPlan(parsed);
      setExecutionPlanError(roleBudgetError(parsed));
    } catch {
      setExecutionPlanError("The execution plan must be valid JSON before validation or launch.");
    }
  }
  const planError = executionPlanError ?? roleBudgetError(blueprint.execution_plan) ?? (!blueprint.execution_plan
    ? "Every Mission needs a reviewed capability plan. For one agent, use roles=[] and a native principal synthesis; do not remove the entire plan."
    : null);
  return (
    <form action={formAction} className="space-y-5">
      <input type="hidden" name="objective" value={objective} />
      <input type="hidden" name="tier" value={tier} />
      <input type="hidden" name="budget_tokens" value={budgetTokens} />
      <input type="hidden" name="budget_scope" value="GovernedInference" />
      <input type="hidden" name="launch" value={launch ? "on" : "off"} />
      <input type="hidden" name="blueprint_json" value={JSON.stringify(blueprint)} />
      <input type="hidden" name="delegation_json" value={JSON.stringify(delegation)} />

      {canQualify && <input type="hidden" name="qualification_run" value={qualificationRun ? "on" : "off"} />}
      <JourneyRail current={launch ? "launch" : "review"} />
      <QualificationChoice canQualify={canQualify} checked={qualificationRun} onChange={changeQualification} />

      {rationale !== null || proposed ? (
        <EnvelopeReveal
          blueprint={blueprint}
          tier={tier}
          budgetTokens={budgetTokens}
          rationale={rationale}
          source={composeSource}
          recommended={recommendedRoute}
          modelBasis={modelBasis}
          delegation={delegation}
        />
      ) : null}
      {composeNote && (
        <div className="rounded-xl border border-border bg-surface-muted/50 p-4 text-sm text-foreground-muted">
          <CompositionNote detail={composeNote} />
        </div>
      )}

      <PackageSection title="Objective" subtitle="Restate it clearly — edit if Bridge misread you.">
        <textarea
          value={objective}
          onChange={(e) => setObjective(e.target.value)}
          rows={3}
          className="w-full resize-y rounded-lg border border-border bg-surface px-3 py-2 text-sm leading-relaxed focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
        />
      </PackageSection>

      <details open className="group rounded-xl border border-border bg-surface-muted/20 [&_summary::-webkit-details-marker]:hidden">
        <summary className="flex cursor-pointer items-center justify-between gap-3 px-5 py-4 text-sm">
          <span className="min-w-0">
            <span className="font-medium">Composed package — review &amp; edit</span>
            <span className="ml-2 text-xs text-foreground-muted">
              Model &amp; harness, instructions, tools, network, isolation, memory — composed for you. Every field is editable; collapse if you just want the defaults.
            </span>
          </span>
          <span aria-hidden className="shrink-0 text-foreground-muted transition-transform group-open:rotate-90">▸</span>
        </summary>
        <div className="space-y-5 border-t border-border p-4">

      <PackageSection
        title="Model & harness"
        subtitle="What the mission reasons with, and the agent runtime it runs on."
      >
        {options.provider && (
          <div className="mb-4 flex items-start gap-3 rounded-lg border border-border bg-surface-muted/40 px-3 py-2.5">
            <Icon name="link" size={16} />
            <div className="min-w-0">
              <p className="text-xs font-medium">
                Cluster default provider:{" "}
                <span className="text-foreground">{options.provider.label}</span>
                <span className="ml-1.5 rounded bg-surface px-1.5 py-0.5 text-[10px] font-normal text-foreground-muted">
                  inherited
                </span>
              </p>
              <p className="mt-0.5 text-[11px] text-foreground-muted">{options.provider.note}</p>
            </div>
          </div>
        )}
        <div className="grid gap-4 sm:grid-cols-2">
          <div>
            <label className="text-xs font-medium text-foreground-muted">Model</label>
            {options.models.length === 0 ? (
              <p className="mt-1.5 rounded-lg bg-surface-muted px-3 py-2 text-xs text-foreground-muted">
                No models are listed for this cluster — the mission will use the configured default
                {options.default_model ? ` (${options.default_model})` : ""}.
              </p>
            ) : (
              <select
                value={model}
                onChange={(event) => {
                  const route = event.target.value;
                  setModel(route);
                  setModelFallbacks((current) => current.filter((fallback) => fallback !== route));
                }}
                className="mt-1.5 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
              >
                {options.models.map((m) => (
                  <option key={modelKey(m.provider, m.deployment)} value={modelKey(m.provider, m.deployment)}>
                    {m.deployment} · {m.provider}
                    {m.is_default ? " (default)" : ""}
                  </option>
                ))}
              </select>
            )}
            <p className="mt-1 text-[11px] text-foreground-muted">
              Select a model from the connected providers without changing the cluster default.
              Pre-flight checks the selected route before launch.
            </p>
            <label className="mt-3 block text-xs text-foreground-muted">
              Qualified fallback routes
              <select
                multiple
                value={modelFallbacks}
                onChange={(event) => {
                  const selected = new Set(
                    Array.from(event.currentTarget.selectedOptions, (option) => option.value),
                  );
                  setModelFallbacks((current) => [
                    ...current.filter((route) => selected.has(route)),
                    ...Array.from(selected).filter((route) => !current.includes(route)),
                  ].slice(0, 8));
                }}
                className="mt-1.5 min-h-24 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm"
              >
                {options.models
                  .map((option) => modelKey(option.provider, option.deployment))
                  .filter((route) => route !== model)
                  .map((route) => (
                    <option key={route} value={route}>
                      {route}
                    </option>
                  ))}
              </select>
              {modelFallbacks.map((route, index) => (
                <span key={route} className="mt-1 flex items-center gap-1 rounded border border-border bg-surface px-2 py-1">
                  <span className="min-w-0 flex-1 truncate">{index + 1}. {route}</span>
                  <button type="button" aria-label={`Move ${route} earlier`} disabled={index === 0} onClick={() => setModelFallbacks((current) => moveFallback(current, index, -1))}>↑</button>
                  <button type="button" aria-label={`Move ${route} later`} disabled={index === modelFallbacks.length - 1} onClick={() => setModelFallbacks((current) => moveFallback(current, index, 1))}>↓</button>
                </span>
              ))}
              <span className="mt-1 block text-[11px]">
                Preflight rejects any fallback that lacks atomic evidence for this exact package and its selected resources.
              </span>
            </label>
            {recommendedModel && (
              <div className="mt-2 flex items-start gap-2 rounded-lg border border-signal/30 bg-signal/5 px-2.5 py-2">
                <Icon name="lightbulb" size={14} />
                <div className="min-w-0 text-[11px]">
                  <p className="font-medium text-foreground">
                    {efficiency?.recommended_low_confidence
                      ? "Insufficient evidence for automatic recommendation"
                      : "Recommended by the efficiency frontier"}
                  </p>
                  <p className="mt-0.5 text-foreground-muted">
                    {recommendedModel.deployment}
                    {recommendedStats
                      ? ` — ${Math.round(recommendedStats.acceptance_rate * 100)}% accepted across ${recommendedStats.runs} run${recommendedStats.runs === 1 ? "" : "s"} on ${recommendedStats.harness || "unrecorded harness"}, ${recommendedStats.tokens_per_outcome?.toLocaleString() ?? "unknown"} tokens/measured delivered outcome`
                      : " — learned from completed runs on this cluster"}
                    {efficiency?.recommended_low_confidence
                      ? " — not selected automatically"
                      : ""}
                  </p>
                </div>
              </div>
            )}
          </div>
          <div>
            <label className="text-xs font-medium text-foreground-muted">Harness</label>
            <select
              value={runtime}
              onChange={(e) => changeRuntime(e.target.value)}
              className="mt-1.5 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
            >
              {(() => {
                const rts = options.runtimes.length
                  ? options.runtimes
                  : [{ kind: "OpenClaw", label: "OpenClaw", wired: true, status: "ready" as const, note: "" }];
                const ready = rts.filter((r) => r.status === "ready");
                const needsImage = rts.filter((r) => r.status === "needs_image");
                const unavailable = rts.filter((r) => r.status === "unavailable");
                const opt = (r: (typeof rts)[number]) => (
                  <option key={r.kind} value={r.kind} disabled={!r.wired}>
                    {r.label}
                    {r.status === "needs_image" ? " — image not configured here" : r.status === "unavailable" ? " — not available" : ""}
                  </option>
                );
                return (
                  <>
                    {ready.length > 0 && <optgroup label="Ready on this cluster">{ready.map(opt)}</optgroup>}
                    {needsImage.length > 0 && <optgroup label="Supported — needs runtime image">{needsImage.map(opt)}</optgroup>}
                    {unavailable.length > 0 && <optgroup label="Not available yet">{unavailable.map(opt)}</optgroup>}
                  </>
                );
              })()}
            </select>
            <p className="mt-1 text-[11px] text-foreground-muted">
              {options.runtimes.filter((r) => r.wired).length} harness{options.runtimes.filter((r) => r.wired).length === 1 ? "" : "es"} can run on this cluster right now. Others are supported by the runtime but need their image configured by an operator. Team members can each use a different ready harness.
            </p>
          </div>
        </div>
      </PackageSection>

      <PackageSection
        title="Instructions"
        subtitle="The mission's system prompt — how it should behave, in addition to the objective."
      >
        <textarea
          value={instructions}
          onChange={(e) => setInstructions(e.target.value)}
          rows={4}
          placeholder="e.g. Be meticulous. Verify every claim against a primary source and cite file paths. Never change code without a passing test."
          className="w-full resize-y rounded-lg border border-border bg-surface px-3 py-2 text-sm leading-relaxed placeholder:text-foreground-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
        />
      </PackageSection>

      <PackageSection
        title="Execution plan"
        subtitle="Roles, dependencies, phases, capabilities, tool-call bounds, synthesis, and deliverables. This is typed and runtime-neutral."
      >
        <textarea
          aria-label="Execution plan"
          value={executionPlanDraft}
          onChange={(event) => changeExecutionPlan(event.target.value)}
          rows={18}
          spellCheck={false}
          className="w-full resize-y rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs leading-relaxed focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
        />
        {planError && (
          <p className="mt-2 text-xs text-danger">{planError}</p>
        )}
      </PackageSection>

      {loopDirective.trim() && (
        <PackageSection
          title="Loop — operating contract"
          subtitle="The feedback loop the harness runs and any sub-agents inherit. Composed from the loop you reviewed."
        >
          <div className="flex items-center justify-between gap-3">
            <p className="text-xs text-foreground-muted">
              This loop is part of the launched package (folded into the agent&rsquo;s instructions).
            </p>
            {cameViaLoopReview && (
              <button
                type="button"
                onClick={goBackToLoopReview}
                className="shrink-0 rounded-lg border border-accent/40 bg-accent/[0.06] px-3 py-1.5 text-xs font-medium text-accent transition hover:bg-accent/10"
              >
                Adjust loop →
              </button>
            )}
          </div>
          <pre className="mt-2 max-h-56 overflow-auto whitespace-pre-wrap rounded-lg border border-border bg-surface p-3 font-mono text-[11px] leading-relaxed text-foreground">
            {loopDirective.trim()}
          </pre>
        </PackageSection>
      )}

      <PackageSection
        title="Tools & connected services"
        subtitle="The tool policy that bounds what it may call, and the MCP services it may use."
      >
        <label className="text-xs font-medium text-foreground-muted">Tool policy</label>
        <select
          value={toolPolicy}
          onChange={(e) => setToolPolicy(e.target.value)}
          className="mt-1.5 w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
        >
          <option value="">Default — kars-default (governed tools permitted)</option>
          {options.tool_policies.map((t) => (
            <option key={t.name} value={t.name}>
              {t.name}
              {t.summary ? ` · ${t.summary}` : ""}
            </option>
          ))}
        </select>
        <p className="mt-1.5 text-xs text-foreground-muted">
          Leaving this at Default selects kars-default, which permits governed tools, including
          shell and Foundry tools. It does not disable tools. Instructions to avoid tools are
          not a no-tools policy; review the selected policy before launch.
        </p>

        <div className="mt-4">
          <label className="text-xs font-medium text-foreground-muted">Connected services (MCP)</label>
          {options.mcp_profiles.length > 0 && (
            <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
              <span className="text-[11px] text-foreground-muted">Vetted bundles:</span>
              {options.mcp_profiles.map((prof) => {
                const active = prof.servers.length > 0 && prof.servers.every((s) => mcp.includes(s));
                return (
                  <button
                    key={prof.name}
                    type="button"
                    title={prof.summary ?? `${prof.servers.length} server(s): ${prof.servers.join(", ")}`}
                    onClick={() =>
                      setMcp((cur) =>
                        active
                          ? cur.filter((x) => !prof.servers.includes(x))
                          : [...new Set([...cur, ...prof.servers])],
                      )
                    }
                    className={`rounded-full border px-2.5 py-0.5 text-[11px] font-medium ${active ? "border-signal/40 bg-signal/10 text-signal" : "border-border text-foreground-muted hover:text-foreground"}`}
                  >
                    {active ? "✓ " : "+ "}{prof.name}
                  </button>
                );
              })}
            </div>
          )}
          {options.mcp_servers.length === 0 ? (
            <p className="mt-1.5 text-xs text-foreground-muted">
              No services are connected. Connect MCP servers in the Operator Console to give the
              mission more tools.
            </p>
          ) : (
            <ul className="mt-1.5 space-y-1.5">
              {options.mcp_servers.map((m) => {
                const checked = mcp.includes(m.name);
                return (
                  <li key={m.name}>
                    <label className="flex items-center gap-2.5 text-sm">
                      <input
                        type="checkbox"
                        checked={checked}
                        onChange={(e) =>
                          setMcp((cur) =>
                            e.target.checked ? [...cur, m.name] : cur.filter((x) => x !== m.name),
                          )
                        }
                        className="h-4 w-4 accent-[var(--signal)]"
                      />
                      <span className="font-medium">{humanizeMcp(m.name)}</span>
                      {m.summary && <span className="text-xs text-foreground-muted">{m.summary}</span>}
                    </label>
                  </li>
                );
              })}
            </ul>
          )}
          {mcpNeedsPolicy && (
            <p role="alert" className="mt-2 rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-warning">
              Connected services need a tool policy to bound them. Select a tool policy above, or
              clear the services.
            </p>
          )}
        </div>
        <div className="mt-4">
          <label className="text-xs font-medium text-foreground-muted">Approved skills</label>
          {options.skills.length === 0 ? (
            <p className="mt-1.5 text-xs text-foreground-muted">No approved skills are available.</p>
          ) : (
            <ul className="mt-1.5 space-y-1.5">
              {options.skills.map((skill) => (
                <li key={skill.name}>
                  <label className="flex items-center gap-2.5 text-sm">
                    <input
                      type="checkbox"
                      checked={skills.includes(skill.name)}
                      onChange={(e) =>
                        setSkills((current) =>
                          e.target.checked
                            ? [...current, skill.name]
                            : current.filter((name) => name !== skill.name),
                        )
                      }
                      className="h-4 w-4 accent-[var(--signal)]"
                    />
                    <span className="font-medium">{skill.name}</span>
                    {skill.summary && (
                      <span className="text-xs text-foreground-muted">{skill.summary}</span>
                    )}
                  </label>
                </li>
              ))}
            </ul>
          )}
        </div>
      </PackageSection>

      <PackageSection
        title="Network egress"
        subtitle="Exactly which external hosts the mission may reach. Empty means no extra egress beyond the model path."
      >
        <p className="mb-3 text-xs text-foreground-muted">
          Strict · reviewed hosts only. Governed model and connected-service access remain subject to their policies.
          Request approved access during a run for new destinations. Datapath observations do not grant access.
        </p>
        <EgressEditor egress={egress} onChange={setEgress} />
      </PackageSection>

      <PackageSection title="Isolation" subtitle="The sandbox hardening the mission runs inside.">
        <div className="space-y-1.5">
          {(options.isolation.length
            ? options.isolation
            : [{ value: "standard", label: "Standard", note: "" }]
          ).map((iso) => (
            <label key={iso.value} className="flex items-start gap-2.5 text-sm">
              <input
                type="radio"
                name="isolation_radio"
                checked={isolation === iso.value}
                onChange={() => setIsolation(iso.value)}
                className="mt-0.5 h-4 w-4 accent-[var(--signal)]"
              />
              <span>
                <span className="font-medium">{iso.label}</span>
                {iso.note && <span className="ml-1.5 text-xs text-foreground-muted">{iso.note}</span>}
              </span>
            </label>
          ))}
        </div>
      </PackageSection>

      {options.memories.length > 0 && (
        <PackageSection
          title="Shared memory"
          subtitle="A shared knowledge store the mission reads and writes (optional)."
        >
          <select
            value={memory}
            onChange={(e) => setMemory(e.target.value)}
            aria-label="Shared memory store"
            className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
          >
            <option value="">None — this mission keeps its own context</option>
            {options.memories.map((m) => (
              <option key={m.name} value={m.name}>
                {m.name}
                {m.summary ? ` · ${m.summary}` : ""}
              </option>
            ))}
          </select>
        </PackageSection>
      )}
        </div>
      </details>

      <PackageSection title="Autonomy" subtitle="How much the mission may do on its own.">
        <SegmentedTier name="tier_display" value={tier} onChange={setTier} />
        <p className="mt-3 rounded-lg bg-surface-muted px-3 py-2 text-xs text-foreground-muted">
          {TIER_CONSEQUENCE[tier]}
          {" "}Delegated sub-roles may hold at most <span className="font-medium text-foreground">Tier {ceiling}</span> — one below the mission.
        </p>
      </PackageSection>

      <PackageSection title="Spending limits" subtitle="Existing workspace rules apply automatically. Add a limit only if you want a tighter cap for this Mission and its roles, including retries and reruns.">
        <label htmlFor="mission-budget-tokens" className="mb-2 block text-sm font-medium">Total token limit (optional)</label>
        <div className="flex flex-wrap items-center gap-2">
          <input
            id="mission-budget-tokens"
            aria-describedby="mission-budget-help"
            type="number"
            min={1}
            step={1}
            value={budgetTokens}
            onChange={(e) => setBudgetTokens(e.target.value)}
            placeholder="Use existing controls"
            className="w-48 rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
          />
          <span className="text-xs text-foreground-muted">tokens — leave blank to use existing controls</span>
        </div>
        <p id="mission-budget-help" className="mt-2 max-w-prose text-sm leading-relaxed text-foreground-muted">
          Tokens measure text the AI reads and writes; they are not dollars. Blank means no shared
          token limit is requested here. Role, profile and workspace limits still apply, and usage
          is always recorded. This does not limit tools or infrastructure costs. Confirm any limit
          here, even if you mentioned it in your request. Estimates do not change these settings.
        </p>
        <RoleBudgetFields draft={executionPlanDraft} onChange={changeExecutionPlan} idPrefix="mission-role-budget" />
        {planError && <p role="alert" className="mt-2 text-xs text-danger">{planError}</p>}
      </PackageSection>

      <PackageSection title="Governance envelope" subtitle="The hard limits this mission runs under.">
        <ul className="space-y-1 text-sm text-foreground-muted">
          <li>• Acts at <span className="font-medium text-foreground">Tier {tier}</span> autonomy.</li>
          <li>• Delegated sub-roles can hold at most <span className="font-medium text-foreground">Tier {ceiling}</span> — never more than the mission.</li>
          {tier <= 3 && (
            <li>• Pauses for your approval before any priced, external, or irreversible action.</li>
          )}
          <li>• Reaches only the {egress.length === 0 ? "model path" : `${egress.length} host${egress.length === 1 ? "" : "s"} you allowed`}; all other egress is denied at the sandbox boundary.</li>
          <li>• Every decision and steer is recorded in a signed Governance Receipt.</li>
        </ul>
      </PackageSection>

      {/* Pre-flight validation (§20) — prove launch-ready before anything runs. */}
      <section className="rounded-2xl border border-border bg-surface p-5 shadow-sm">
        <div className="flex flex-col items-start justify-between gap-3 sm:flex-row">
          <div>
            <h2 className="text-sm font-semibold">Check before starting</h2>
            <p className="mt-1 max-w-prose text-sm leading-relaxed text-foreground-muted">
              Check that your chosen AI, connected services, permissions and spending settings
              are ready. This does not start the work or test the quality of its results.
            </p>
          </div>
          <button
            type="button"
            onClick={runValidation}
            disabled={validating || mcpNeedsPolicy || planError !== null}
            className="shrink-0 rounded-lg border border-border bg-surface px-3 py-1.5 text-xs font-medium transition hover:bg-surface-muted disabled:opacity-50"
          >
            {validating ? "Checking…" : validation || validationError ? "Check again" : "Check readiness"}
          </button>
        </div>
        {/* Validation animates the same orchestration cube with a live feed of
            what's being checked against the live cluster — so validate feels as
            alive as compose, and the Execute button only appears once green. */}
        {validating && (
          <div className="mt-4">
            <OrchestrationCube
              title="Validating against the live cluster"
              done={false}
              active={0}
              phases={[
                { icon: "brain", label: "Resolving the model on the cluster", detail: model ? model.split("::")[1] ?? model : "controller default" },
                { icon: "wrench", label: "Checking tool policy + connected services", detail: `${mcp.length} service${mcp.length === 1 ? "" : "s"}${toolPolicy ? ` · ${toolPolicy}` : ""}` },
                { icon: "globe", label: "Verifying egress reachability", detail: egress.length ? egress.map((e) => e.host).slice(0, 3).join(", ") : "model path only" },
                { icon: "shield", label: "Proving the envelope is launch-ready", detail: `Tier ${tier} · capability + budget checks` },
              ]}
            />
          </div>
        )}
        {/* Loud, honest feedback in every branch — never a dead button. */}
        {mcpNeedsPolicy && (
          <p className="mt-3 rounded-lg border border-warning/40 bg-warning/10 px-3 py-2 text-xs text-warning">
            Connected services (MCP) require a tool policy to bound them. Pick a tool policy above,
            then validate.
          </p>
        )}
        {validationError && (
          <div className="mt-3 rounded-lg border border-danger/40 bg-danger/10 px-3 py-2 text-xs text-danger">
            <span className="font-semibold">Pre-flight could not complete.</span> {validationError}
          </div>
        )}
        {validation && (
          <>
            {!validationFresh && (
              <p className="mt-3 rounded-lg border border-warning/40 bg-warning/10 px-3 py-2 text-xs text-warning">
                You edited the package since this ran — these results are stale. Re-validate to launch.
              </p>
            )}
            <ValidationChecks checks={validation.checks}
              budget={validationFresh ? { blueprint, tokens: budgetTokens.trim() === "" ? null : Number(budgetTokens) } : undefined}
              onReviewBudget={() => document.getElementById("mission-budget-tokens")?.focus()} />
            {validationFresh && !validation.ok && (
              <p className="mt-2 text-xs font-medium text-danger">
                Fix the failing checks above before launching.
              </p>
            )}
          </>
        )}
      </section>

      <RepoAccess selected={selectedRepos} onSelectionChange={changeSelectedRepos}
        readOnly={!missionGithubWriteEnabled(blueprint.execution_plan)} />
      {selectedRepos.length > 0 && !(validationFresh && validation?.ok) && (
        <p className="text-xs text-danger">Check readiness for these repositories before creating a draft. Nothing has started yet.</p>
      )}

      <label className="flex items-center gap-2.5 rounded-lg border border-border bg-surface px-4 py-3 text-sm">
        <input
          type="checkbox"
          checked={launch}
          onChange={(e) => setLaunch(e.target.checked)}
          className="h-4 w-4 accent-[var(--signal)]"
        />
        <span>
          Launch immediately after creating.{" "}
          <span className="text-foreground-muted">
            Leave unchecked to create a governed draft you launch when ready.
          </span>
        </span>
      </label>

      {state.error && (
        <p role="alert" className="rounded-lg border border-danger/30 bg-danger/10 px-4 py-3 text-sm text-danger">
          {state.error}
        </p>
      )}

      <div className="flex items-center justify-between rounded-xl border border-border bg-surface-muted/50 px-4 py-3">
        <p className="text-xs font-medium text-foreground-muted">Nothing has started yet.</p>
        <div className="flex items-center gap-3">
          <button
            type="button"
            onClick={goBack}
            className="rounded-lg px-3 py-2 text-sm text-foreground-muted hover:text-foreground"
          >
            {cameViaLoopReview ? "← Back to loop" : "← Back"}
          </button>
          <CreateButton
            disabled={
              mcpNeedsPolicy
              || planError !== null
              || ((launch || selectedRepos.length > 0) && !(validationFresh && validation?.ok))
            }
            launch={launch}
            needsValidation={(launch || selectedRepos.length > 0) && !(validationFresh && validation?.ok === true)}
          />
        </div>
      </div>
    </form>
  );
}
