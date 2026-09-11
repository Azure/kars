"use client";

// kars Bridge Workspace — mission intake → editable launch package → launch.
//
// The design note's §20 flow in plain language: describe what you want, review
// a complete package composed from REAL cluster facts (the models this cluster
// serves, the harnesses it can run, the tool policies / connected services /
// shared memory that exist), edit any of it, then deliberately Launch. Honesty:
// the package is a deterministic sensible-defaults starting point you review —
// we do not claim an AI composed it (the intake orchestrator is itself a
// governed run, surfaced when the run environment is connected). Every control
// maps to a real field the controller compiles into the InferencePolicy +
// KarsSandbox.

import { useEffect, useMemo, useRef, useState } from "react";
import { useFormStatus } from "react-dom";
import { useActionState } from "react";
import { SegmentedTier } from "@/components/segmented-tier";
import { OrchestrationCube } from "@/components/orchestration-cube";
import { Icon } from "@/components/icon";
import { JourneyRail } from "@/components/journey-rail";
import { humanizeMcp } from "@/lib/format";
import { EnvelopeReveal } from "./envelope-reveal";
import { LoopDesigner } from "@/components/loop-designer";
import { RepoAccess } from "@/components/repo-access";
import {
  createMissionAction,
  validateMissionAction,
  composeMissionAction,
  proposeLoopAction,
  type IntakeState,
} from "./actions";
import type {
  Blueprint,
  BlueprintEgress,
  ComposeProposal,
  Efficiency,
  Options,
  ValidationResult,
} from "@/lib/types";

const TIER_CONSEQUENCE: Record<number, string> = {
  1: "Manual — the mission proposes every step and does nothing on its own. You perform each action.",
  2: "Shared — the mission acts only on low-risk steps; everything else waits for your approval.",
  3: "Conditional — the mission acts on its own but pauses for your approval before anything that costs money, touches external systems, or can't be undone.",
  4: "Supervised — the mission runs autonomously with periodic checkpoints you sign off on.",
  5: "Full — the mission runs autonomously within its budget and time limit; you review the result.",
};

const EXAMPLES = [
  "Audit our README for outdated install steps and propose fixes.",
  "Draft a competitive teardown of the top 3 agent platforms.",
  "Summarize this contract's risk and obligations.",
];

function modelKey(provider: string, deployment: string) {
  return `${provider}::${deployment}`;
}

function moveFallback(routes: string[], index: number, delta: number): string[] {
  const next = index + delta;
  if (next < 0 || next >= routes.length) return routes;
  const copy = [...routes];
  [copy[index], copy[next]] = [copy[next], copy[index]];
  return copy;
}

export function IntakeFlow({ options, efficiency, initialObjective }: { options: Options; efficiency?: Efficiency | null; initialObjective?: string }) {
  const [state, formAction] = useActionState<IntakeState, FormData>(
    createMissionAction,
    { error: null },
  );
  const [objective, setObjective] = useState(initialObjective ?? "");
  const [proposed, setProposed] = useState(false);
  // Loop-review step: when arriving with an intent, the orchestrator proposes a
  // loop the user reviews here BEFORE we compose the package.
  const [loopProposal, setLoopProposal] = useState<import("@/lib/types").LoopProposal | null>(null);
  const [loopLoading, setLoopLoading] = useState(false);
  const [loopReviewed, setLoopReviewed] = useState(false);

  // The learned efficiency frontier drives the recommendation. The recommended
  // route (e.g. `azure-openai/openai/gpt-4o`) carries the deployment; match it
  // back to a real model option and surface its stats at the point of choice.
  const recommendedRouteRaw = efficiency?.recommended ?? null;
  const recommendedModel = recommendedRouteRaw
    ? options.models.find((m) => recommendedRouteRaw.includes(m.deployment)) ?? null
    : null;
  const recommendedStats = recommendedRouteRaw
    ? efficiency?.routes.find((r) => r.route === recommendedRouteRaw) ?? null
    : null;
  const actionableRecommendedModel =
    efficiency?.recommended_low_confidence ? null : recommendedModel;

  // ── Composition state (the editable blueprint) ──────────────────────────
  const defaultModel =
    actionableRecommendedModel ?? options.models.find((m) => m.is_default) ?? options.models[0] ?? null;
  const [model, setModel] = useState<string>(
    defaultModel ? modelKey(defaultModel.provider, defaultModel.deployment) : "",
  );
  const [modelFallbacks, setModelFallbacks] = useState<string[]>([]);
  const [runtime, setRuntime] = useState<string>(
    options.runtimes[0]?.kind ?? "OpenClaw",
  );
  const [instructions, setInstructions] = useState("");
  // The loop scaffold (LOOP:/GOAL:/CYCLE/SUB-AGENT INHERITANCE …) the harness
  // runs. It is the agent's OPERATING CONTRACT — never the mission's identity.
  // It is carried into the blueprint's instructions, so it reaches the harness
  // and sub-agents, but it NEVER becomes the objective, display name, or slug.
  const [loopDirective, setLoopDirective] = useState("");
  const [toolPolicy, setToolPolicy] = useState<string>("");
  const [mcp, setMcp] = useState<string[]>([]);
  const [skills, setSkills] = useState<string[]>([]);
  const [egress, setEgress] = useState<BlueprintEgress[]>([]);
  // Egress enforcement mode (§ learning|strict). "strict" enforces the proposed
  // allowlist from the first run; "learning" starts in Learn mode (observe the
  // hosts the agent actually reaches, enforce nothing), so you can enforce the
  // learned set after review. Maps to the controller's Strict-vs-Learn network
  // policy: a strict launch carries the allowlist, a learning launch carries none.
  const [egressMode, setEgressMode] = useState<"strict" | "learning">("strict");
  const [isolation, setIsolation] = useState<string>(
    options.isolation[0]?.value ?? "standard",
  );
  const [memory, setMemory] = useState<string>("");
  const [delegation, setDelegation] = useState<import("@/lib/types").MissionDelegation>({
    mode: "single-agent",
    roles: [],
    max_parallel: 1,
  });
  const [executionPlan, setExecutionPlan] = useState<import("@/lib/types").ExecutionPlan | null>(
    null,
  );
  const [executionPlanDraft, setExecutionPlanDraft] = useState("");
  const [executionPlanError, setExecutionPlanError] = useState<string | null>(null);

  // ── Governance state ────────────────────────────────────────────────────
  const [tier, setTier] = useState(3);
  const [budgetTokens, setBudgetTokens] = useState<string>("");
  const [launch, setLaunch] = useState(false);

  function changeRuntime(nextRuntime: string) {
    setRuntime(nextRuntime);
    if (nextRuntime === "BYO") {
      setDelegation({ mode: "single-agent", roles: [], max_parallel: 1 });
      setExecutionPlan(null);
      setExecutionPlanDraft("");
    }
    if (nextRuntime === "OpenClaw") return;
    setSkills([]);
    if (toolPolicy === "kars-team-member") setToolPolicy("kars-default");
    const repositorySecurity =
      nextRuntime === "Hermes" &&
      mcp.some((server) => server.toLowerCase() === "github") &&
      egress.some((endpoint) => endpoint.host.toLowerCase() === "api.osv.dev");
    if (
      repositorySecurity &&
      (budgetTokens.trim() === "" || Number(budgetTokens) < 600_000)
    ) {
      setBudgetTokens("600000");
    }
  }

  // ── Orchestrator (intent → composed package) ────────────────────────────
  const [composing, setComposing] = useState(false);
  const [rationale, setRationale] = useState<string | null>(null);
  const [composeSource, setComposeSource] = useState<string | null>(null);
  const [modelBasis, setModelBasis] = useState<string | null>(null);
  const [composeNote, setComposeNote] = useState<string | null>(null);

  function applyProposal(p: ComposeProposal) {
    if (p.model) setModel(modelKey(p.model.provider, p.model.deployment));
    setModelFallbacks(
      (p.model_fallbacks ?? []).map((route) => modelKey(route.provider, route.deployment)),
    );
    if (p.runtime) setRuntime(p.runtime);
    setInstructions(p.instructions ?? "");
    setToolPolicy(p.tool_policy ?? "");
    setMcp(p.mcp_servers ?? []);
    setSkills(p.skills ?? []);
    setEgress(p.egress ?? []);
    if (p.isolation) setIsolation(p.isolation);
    setMemory(p.memory ?? "");
    setDelegation(
      p.delegation ?? { mode: "single-agent", roles: [], max_parallel: 1 },
    );
    setExecutionPlan(p.execution_plan ?? null);
    setExecutionPlanDraft(
      p.execution_plan ? JSON.stringify(p.execution_plan, null, 2) : "",
    );
    setExecutionPlanError(null);
    if (p.tier) setTier(Math.min(5, Math.max(1, p.tier)));
    setBudgetTokens(p.budget_tokens != null ? String(p.budget_tokens) : "");
  }

  async function composeAndReview(objectiveOverride?: string) {
    const obj = objectiveOverride ?? objective;
    setComposing(true);
    setComposeNote(null);
    setRationale(null);
    setComposeSource(null);
    try {
      const res = await composeMissionAction(obj);
      if (res.available && res.proposal) {
        applyProposal(res.proposal);
        setRationale(res.rationale);
        setComposeSource(res.source);
        setModelBasis(res.proposal.model_basis ?? null);
        setProposed(true);
      } else {
        setComposeNote(
          res.reason ??
            "The AI orchestrator isn't available. Retry composition before reviewing or validating a package.",
        );
      }
    } catch {
      setComposeNote(
        "Couldn't reach the orchestrator. Retry composition before reviewing or validating a package.",
      );
    } finally {
      setComposing(false);
    }
  }

  // Unified intake: when arriving with a prefilled intent, first ask the
  // ORCHESTRATOR to propose a loop for the user to review (not straight to
  // compose). Once reviewed + applied, composeAndReview runs on the loop-shaped
  // objective. If the loop step is skipped, we compose the raw intent.
  const autoRan = useRef(false);
  useEffect(() => {
    if (!autoRan.current && initialObjective && initialObjective.trim().length > 0) {
      autoRan.current = true;
      setLoopLoading(true);
      void proposeLoopAction(initialObjective.trim(), "mission")
        .then((p) => setLoopProposal(p))
        .finally(() => setLoopLoading(false));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Apply the reviewed loop → the loop text becomes the agent's OPERATING
  // CONTRACT (carried in instructions), NOT the objective. The objective stays
  // the human's plain intent so the title, URL, and sandbox name stay clean.
  // We also fold the user's edits back into loopProposal so returning to the
  // loop-review step (via Back / Adjust loop) shows exactly what they applied.
  function applyLoopAndCompose(
    text: string,
    parts?: { goal: string; patternId: string; criteria: string },
  ) {
    const humanIntent = (initialObjective ?? objective).trim();
    setLoopDirective(text);
    if (parts) {
      setLoopProposal((prev) => ({
        pattern: parts.patternId,
        goal: parts.goal,
        criteria: parts.criteria,
        rationale: prev?.rationale ?? "",
        source: prev?.source ?? "heuristic",
      }));
    }
    setObjective(humanIntent);
    setLoopReviewed(true);
    void composeAndReview(humanIntent);
  }
  function skipLoop() {
    setLoopReviewed(true);
    if (initialObjective && initialObjective.trim().length > 0) {
      void composeAndReview(initialObjective.trim());
    }
  }

  // Coherent back-navigation from the review page. The intent-first path came
  // through the loop-review step, so Back must return THERE (loop preserved) —
  // not dump the user at the intent box ("the beginning"). The manual path has
  // no loop-review step, so Back returns to intent capture.
  const cameViaLoopReview = !!(initialObjective && initialObjective.trim().length > 0);
  function goBackToLoopReview() {
    setProposed(false);
    setLoopReviewed(false);
  }
  function goBack() {
    setProposed(false);
    if (cameViaLoopReview) setLoopReviewed(false);
  }

  // ── Pre-flight validation (§20) ─────────────────────────────────────────
  const [validation, setValidation] = useState<ValidationResult | null>(null);
  const [validatedSig, setValidatedSig] = useState<string | null>(null);
  const [validating, setValidating] = useState(false);
  const [validationError, setValidationError] = useState<string | null>(null);

  const ceiling = Math.max(1, tier - 1);
  const recommendedRoute = actionableRecommendedModel?.deployment ?? null;

  // MCP access requires a tool policy to bound it (the substrate enforces this
  // at admission; we surface it here so the user fixes it before launch).
  const mcpNeedsPolicy = mcp.length > 0 && toolPolicy === "";

  const blueprint: Blueprint = useMemo(() => {
    const [provider, deployment] = model.split("::");
    const bp: Blueprint = { runtime, isolation };
    if (provider && deployment) bp.model = { provider, deployment };
    bp.model_fallbacks = modelFallbacks.map((route) => {
      const [fallbackProvider, fallbackDeployment] = route.split("::");
      return { provider: fallbackProvider, deployment: fallbackDeployment };
    });
    // Instructions + the loop operating contract both feed the harness. The
    // loop scaffold is appended here (never to the objective/title) so the
    // agent runs the loop and sub-agents inherit it, while identity stays clean.
    const mergedInstructions = [instructions.trim(), loopDirective.trim()]
      .filter(Boolean)
      .join("\n\n");
    if (mergedInstructions) bp.instructions = mergedInstructions;
    if (toolPolicy) bp.tool_policy = toolPolicy;
    if (mcp.length) bp.mcp_servers = mcp;
    if (skills.length) bp.skills = skills;
    // Strict enforces the proposed allowlist; learning launches in Learn mode
    // (no allowlist → the controller observes egress instead of blocking it).
    if (egressMode === "strict" && egress.length) bp.egress = egress;
    bp.egress_mode = egressMode;
    if (memory) bp.memory = memory;
    if (executionPlan) bp.execution_plan = executionPlan;
    return bp;
  }, [model, modelFallbacks, runtime, instructions, loopDirective, toolPolicy, mcp, skills, egress, egressMode, isolation, memory, executionPlan]);

  // A validation is only "fresh" for the exact package it was run against — any
  // edit (composition, tier, or budget) makes the prior result stale, so you
  // must re-validate exactly what you'll launch.
  const packageSig = useMemo(
    () => JSON.stringify({ blueprint, tier, budgetTokens, executionPlanDraft }),
    [blueprint, tier, budgetTokens, executionPlanDraft],
  );
  const validationFresh = validation != null && validatedSig === packageSig;

  async function runValidation() {
    setValidating(true);
    setValidationError(null);
    try {
      const res = await validateMissionAction(blueprint, {
        tier,
        budget_tokens: budgetTokens.trim() === "" ? null : Number(budgetTokens),
      });
      if (!res || !Array.isArray(res.checks)) {
        throw new Error("The pre-flight service returned an unexpected response (no checks).");
      }
      setValidation(res);
      setValidatedSig(packageSig);
    } catch (e) {
      // Fail loud: never leave the user staring at a button with no feedback.
      setValidation(null);
      setValidationError(
        e instanceof Error ? e.message : "Pre-flight validation failed — the cluster could not be reached.",
      );
    } finally {
      setValidating(false);
    }
  }

  // Step 1a — LOOP REVIEW: arrived with an intent → the orchestrator proposed a
  // loop; the user reviews/edits it here before we compose. Runs before the
  // package compose, and only for the intent-first path.
  if (!proposed && !loopReviewed && initialObjective && initialObjective.trim().length > 0) {
    return (
      <div className="space-y-5">
        <JourneyRail current="describe" />
        <div className="kb-card kb-canvas p-6">
          <h2 className="text-sm font-semibold">Review the loop</h2>
          <p className="mt-1 text-xs text-foreground-muted">
            The orchestrator turned your intent into a feedback loop (2026 loop engineering). Review
            the pattern and success criteria — change anything — then continue; the loop becomes what
            the harness runs and is inherited by any sub-agents.
          </p>
          {loopLoading ? (
            <div className="mt-4 flex items-center gap-2 text-sm text-foreground-muted">
              <span className="h-2 w-2 animate-ping rounded-full bg-accent" /> Orchestrator is defining the loop…
            </div>
          ) : (
            <div className="mt-3">
              <LoopDesigner
                surface="mission"
                defaultOpen
                initialGoal={loopProposal?.goal ?? initialObjective}
                initialPatternId={loopProposal?.pattern}
                initialCriteria={loopProposal?.criteria ?? ""}
                rationale={loopProposal?.rationale}
                applyLabel="Continue with this loop →"
                onApply={applyLoopAndCompose}
              />
              <button
                type="button"
                onClick={skipLoop}
                className="mt-3 text-xs text-foreground-muted underline hover:text-foreground"
              >
                Skip — compose from my plain intent instead
              </button>
            </div>
          )}
        </div>
      </div>
    );
  }

  // Composing (either path): show ONLY the orchestration animation — never fall
  // back to the editable intent form, which reads as "jumped back to the start"
  // the instant the user continues from the loop review.
  if (!proposed && composing) {
    return (
      <div className="space-y-5">
        <JourneyRail current="compose" />
        <OrchestrationCube
          title="Orchestrating your package"
          done={false}
          active={0}
          phases={[
            { icon: "layers", label: "Reading the cluster palette", detail: `${options.models.length} model${options.models.length === 1 ? "" : "s"} · ${options.runtimes.filter((r) => r.wired).length} harness${options.runtimes.filter((r) => r.wired).length === 1 ? "" : "es"} · ${options.tool_policies.length} tool ${options.tool_policies.length === 1 ? "policy" : "policies"}` },
            {
              icon: "chart",
              label: "Consulting the efficiency frontier",
              detail: recommendedModel
                ? efficiency?.recommended_low_confidence
                  ? `limited evidence for ${recommendedModel.deployment}; preserving the cluster default`
                  : `learned recommendation: ${recommendedModel.deployment}`
                : "no completed runs yet — composing from safe defaults",
            },
            { icon: "layers", label: "Assembling the governed package", detail: `${options.mcp_servers.length} connected service${options.mcp_servers.length === 1 ? "" : "s"} · egress · autonomy tier · budget` },
            { icon: "note", label: loopDirective.trim() ? "Folding in your reviewed loop" : "Preparing the proposal for review", detail: (initialObjective ?? objective).trim().slice(0, 72) || "your objective" },
          ]}
        />
      </div>
    );
  }

  // Step 1 — intent capture, two-column: prompt + a preview of what composes.
  if (!proposed) {
    return (
      <div className="space-y-5">
        <JourneyRail current="describe" />
        {composeNote && (
          <div className="rounded-xl border border-danger/30 bg-danger/5 p-4 text-sm text-danger">
            {composeNote}
          </div>
        )}
        <div className="grid gap-5 lg:grid-cols-[1.15fr_0.85fr]">
        <div className="kb-card kb-canvas p-6">
          <label htmlFor="objective" className="text-sm font-semibold">
            What do you want done?
          </label>
          <p className="mt-1 text-xs text-foreground-muted">Describe an outcome. The orchestrator composes a complete, governed package you can edit before anything runs.</p>
          <textarea
            id="objective"
            value={objective}
            onChange={(e) => {
              setObjective(e.target.value);
              // Auto-grow so long objectives are fully visible instead of
              // scrolling inside a fixed 5-row box.
              e.target.style.height = "auto";
              e.target.style.height = `${Math.min(e.target.scrollHeight, 480)}px`;
            }}
            rows={5}
            maxLength={4000}
            autoFocus
            placeholder="e.g. Audit our README for outdated install steps and open a PR with fixes…"
            className="mt-3 w-full resize-y overflow-y-auto rounded-xl border border-border bg-surface px-4 py-3 text-sm leading-relaxed placeholder:text-foreground-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
          />
          <div className="mt-1 text-right text-[11px] tabular-nums text-foreground-muted">
            {objective.length.toLocaleString()} / 4,000
          </div>
          <div className="mt-3 flex flex-wrap gap-2">
            {EXAMPLES.map((ex) => (
              <button
                key={ex}
                type="button"
                onClick={() => setObjective(ex)}
                className="rounded-full border border-border bg-surface-muted px-3 py-1 text-xs text-foreground-muted transition hover:bg-surface hover:text-foreground"
              >
                {ex}
              </button>
            ))}
          </div>
          {/* Loop engineering (2026): design the feedback loop, not a one-shot
              prompt. The generated loop becomes the objective the harness runs
              and is inherited by any sub-agents. */}
          <div className="mt-3">
            <LoopDesigner
              surface="mission"
              initialGoal={objective}
              onApply={(text, parts) => {
                // The loop scaffold is the agent's OPERATING CONTRACT — it folds
                // into the blueprint instructions (via mergedInstructions), and
                // must NEVER become the mission objective/title/slug. Set the
                // directive; only seed the objective from the loop's GOAL when
                // it's still empty, so Compose enables without the scaffold ever
                // becoming the mission identity.
                setLoopDirective(text);
                setLoopReviewed(true);
                if (parts) {
                  setLoopProposal((prev) => ({
                    pattern: parts.patternId,
                    goal: parts.goal,
                    criteria: parts.criteria,
                    rationale: prev?.rationale ?? "",
                    source: prev?.source ?? "heuristic",
                  }));
                }
                if (!objective.trim() && parts?.goal) setObjective(parts.goal);
              }}
            />
          </div>
          <div className="mt-5 flex items-center justify-end">
            <button
              type="button"
              disabled={objective.trim().length === 0 || composing}
              onClick={() => composeAndReview()}
              className="inline-flex items-center gap-2 rounded-lg bg-signal px-5 py-2.5 text-sm font-semibold text-signal-fg shadow-sm transition hover:opacity-90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal disabled:opacity-50"
            >
              {composing ? (
                <>
                  <span className="relative flex h-2 w-2" aria-hidden>
                    <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-signal-fg/70" />
                    <span className="relative inline-flex h-2 w-2 rounded-full bg-signal-fg" />
                  </span>
                  Composing…
                </>
              ) : (
                "Compose the package →"
              )}
            </button>
          </div>
        </div>

        <div className="kb-card p-6">
          <h2 className="text-sm font-semibold">What gets assembled</h2>
          <p className="mt-0.5 text-xs text-foreground-muted">From your intent, kars composes a full trust envelope — and lets you edit every part.</p>
          <ul className={`mt-4 space-y-2.5 ${composing ? "opacity-100" : ""}`}>
            {([
              ["brain", "Model & harness", "a best-fit runtime, informed by past efficiency where available"],
              ["target", "Autonomy tier", "how much it may do before asking you"],
              ["wrench", "Tools & policy", "the bounded set of tools it may call"],
              ["plug", "Connected services", "MCP servers / internal systems it may reach"],
              ["globe", "Network egress", "exact external hosts allowed — all else denied"],
              ["database", "Shared memory", "the knowledge commons it reads + writes"],
            ] as const).map(([icon, t, d]) => (
              <li key={t} className="flex items-start gap-2.5">
                <span className={`mt-0.5 text-foreground-muted ${composing ? "kb-pulse rounded-full" : ""}`} aria-hidden><Icon name={icon} /></span>
                <div>
                  <p className="text-sm font-medium">{t}</p>
                  <p className="text-xs text-foreground-muted">{d}</p>
                </div>
              </li>
            ))}
          </ul>
          <p className="mt-5 rounded-lg bg-surface-muted/60 px-3 py-2 text-[11px] text-foreground-muted">
            Nothing runs at compose time. You review, edit, validate, then explicitly launch.
          </p>
        </div>
        </div>
      </div>
    );
  }

  // Step 2 — the editable package + hard launch gate.
  return (
    <form action={formAction} className="space-y-5">
      <input type="hidden" name="objective" value={objective} />
      <input type="hidden" name="tier" value={tier} />
      <input type="hidden" name="budget_tokens" value={budgetTokens} />
      <input type="hidden" name="launch" value={launch ? "on" : "off"} />
      <input type="hidden" name="blueprint_json" value={JSON.stringify(blueprint)} />
      <input type="hidden" name="delegation_json" value={JSON.stringify(delegation)} />

      <JourneyRail current={launch ? "launch" : "review"} />

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
          {composeNote}
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
                This cluster serves models via{" "}
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
                    {m.deployment}
                    {m.is_default ? " (default)" : ""}
                  </option>
                ))}
              </select>
            )}
            <p className="mt-1 text-[11px] text-foreground-muted">
              A default is pre-selected for the objective; switch to any model
              {options.provider ? ` ${options.provider.label}` : " your cluster"} serves.
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
                      ? ` — ${Math.round(recommendedStats.acceptance_rate * 100)}% accepted across ${recommendedStats.runs} run${recommendedStats.runs === 1 ? "" : "s"}, ${recommendedStats.tokens_per_outcome.toLocaleString()} tokens/outcome`
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
        {executionPlanDraft ? (
          <>
            <textarea
              value={executionPlanDraft}
              onChange={(event) => {
                const next = event.target.value;
                setExecutionPlanDraft(next);
                try {
                  const parsed = JSON.parse(next) as import("@/lib/types").ExecutionPlan;
                  setExecutionPlan(parsed);
                  setExecutionPlanError(null);
                } catch {
                  setExecutionPlanError("The execution plan must be valid JSON before validation or launch.");
                }
              }}
              rows={18}
              spellCheck={false}
              className="w-full resize-y rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs leading-relaxed focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
            />
            {executionPlanError && (
              <p className="mt-2 text-xs text-danger">{executionPlanError}</p>
            )}
          </>
        ) : (
          <p className="text-xs text-foreground-muted">
            Single-agent execution — no worker plan was proposed. Recompose the mission to request decomposition.
          </p>
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
          <option value="">None — model only (no governed tools)</option>
          {options.tool_policies.map((t) => (
            <option key={t.name} value={t.name}>
              {t.name}
              {t.summary ? ` · ${t.summary}` : ""}
            </option>
          ))}
        </select>

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
        <div className="mb-3 inline-flex rounded-lg border border-border bg-surface p-1 text-xs">
          <button
            type="button"
            onClick={() => setEgressMode("strict")}
            className={`rounded-md px-3 py-1.5 font-medium transition ${egressMode === "strict" ? "bg-signal text-signal-fg" : "text-foreground-muted hover:text-foreground"}`}
          >
            Strict
          </button>
          <button
            type="button"
            onClick={() => setEgressMode("learning")}
            className={`rounded-md px-3 py-1.5 font-medium transition ${egressMode === "learning" ? "bg-accent text-accent-fg" : "text-foreground-muted hover:text-foreground"}`}
          >
            Learning
          </button>
        </div>
        <p className="mb-3 text-xs text-foreground-muted">
          {egressMode === "strict"
            ? "Only the hosts below are reachable from the first run — everything else is denied. The safe default."
            : "The mission starts in Learn mode: it observes which hosts the agent actually reaches (nothing is blocked yet), so you can review and enforce the learned set afterward. Use for exploratory work when the host set isn't known up front."}
        </p>
        <div className={egressMode === "learning" ? "opacity-50" : ""}>
          <EgressEditor egress={egress} onChange={setEgress} />
        </div>
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

      <PackageSection title="Budget" subtitle="An optional token ceiling for the whole mission.">
        <div className="flex items-center gap-2">
          <input
            type="number"
            min={0}
            value={budgetTokens}
            onChange={(e) => setBudgetTokens(e.target.value)}
            placeholder="e.g. 200000"
            className="w-48 rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
          />
          <span className="text-xs text-foreground-muted">tokens — leave blank for no cap</span>
        </div>
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
        <div className="flex items-start justify-between gap-3">
          <div>
            <h2 className="text-sm font-semibold">Pre-flight check</h2>
            <p className="mt-0.5 text-xs text-foreground-muted">
              Validate the package against the live cluster before anything runs — tools, services,
              memory, model, and network are checked.
            </p>
          </div>
          <button
            type="button"
            onClick={runValidation}
            disabled={validating || mcpNeedsPolicy || executionPlanError !== null}
            className="shrink-0 rounded-lg border border-border bg-surface px-3 py-1.5 text-xs font-medium transition hover:bg-surface-muted disabled:opacity-50"
          >
            {validating ? "Checking…" : "Validate package"}
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
            <ul className="mt-3 space-y-1.5">
              {validation.checks.map((c) => (
                <li key={c.id} className="flex items-start gap-2 text-sm">
                  <CheckMark status={c.status} />
                  <span>
                    <span className="font-medium">{c.label}</span>
                    <span className="ml-1.5 text-xs text-foreground-muted">{c.detail}</span>
                  </span>
                </li>
              ))}
            </ul>
            {validationFresh && !validation.ok && (
              <p className="mt-2 text-xs font-medium text-danger">
                Fix the failing checks above before launching.
              </p>
            )}
          </>
        )}
      </section>

      <RepoAccess />

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
              || executionPlanError !== null
              || (launch && !(validationFresh && validation!.ok))
            }
            launch={launch}
            needsValidation={launch && !(validationFresh && validation?.ok === true)}
          />
        </div>
      </div>
    </form>
  );
}

function CheckMark({ status }: { status: "pass" | "fail" | "warn" }) {
  const map = {
    pass: { c: "text-ok", s: "✓" },
    warn: { c: "text-warning", s: "!" },
    fail: { c: "text-danger", s: "✕" },
  } as const;
  const m = map[status];
  return (
    <span className={`mt-0.5 inline-flex h-4 w-4 shrink-0 items-center justify-center rounded-full border text-[10px] font-bold ${m.c}`} aria-hidden>
      {m.s}
    </span>
  );
}

function CreateButton({
  disabled,
  launch,
  needsValidation,
}: {
  disabled: boolean;
  launch: boolean;
  needsValidation: boolean;
}) {
  const { pending } = useFormStatus();
  const label = pending
    ? "Creating…"
    : needsValidation
      ? "Validate to launch"
      : launch
        ? "Create & launch"
        : "Create draft";
  return (
    <button
      type="submit"
      disabled={pending || disabled}
      className="rounded-lg bg-signal px-5 py-2.5 text-sm font-semibold text-signal-fg shadow-sm transition hover:opacity-90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal disabled:opacity-50"
    >
      {label}
    </button>
  );
}

function EgressEditor({
  egress,
  onChange,
}: {
  egress: BlueprintEgress[];
  onChange: (e: BlueprintEgress[]) => void;
}) {
  const [host, setHost] = useState("");
  const [port, setPort] = useState("443");
  const [err, setErr] = useState<string | null>(null);

  // A permissive hostname / IPv4 check — rejects schemes, paths, spaces, and
  // obvious junk so a bad allowlist entry can't silently reach the controller.
  const HOST_RE = /^(?:\*\.)?(?:[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?\.)+[a-zA-Z]{2,}$|^(?:\d{1,3}\.){3}\d{1,3}$|^localhost$/;

  function add() {
    setErr(null);
    const h = host.trim().toLowerCase();
    if (!h) return;
    if (h.includes("/") || h.includes(":") || h.includes(" ")) {
      setErr("Enter a bare hostname (no scheme, port, or path) — set the port separately.");
      return;
    }
    if (!HOST_RE.test(h)) {
      setErr("That doesn't look like a valid hostname or IP.");
      return;
    }
    let p: number | null = null;
    if (port.trim() !== "") {
      const n = Number(port);
      if (!Number.isInteger(n) || n < 1 || n > 65535) {
        setErr("Port must be a whole number between 1 and 65535.");
        return;
      }
      p = n;
    }
    if (egress.some((e) => e.host === h && e.port === p)) {
      setErr("That host:port is already in the allowlist.");
      return;
    }
    onChange([...egress, { host: h, port: p }]);
    setHost("");
    setPort("443");
  }

  return (
    <div className="space-y-2">
      {egress.length > 0 && (
        <ul className="space-y-1.5">
          {egress.map((e, i) => (
            <li
              key={`${e.host}:${e.port ?? ""}:${i}`}
              className="flex items-center justify-between rounded-lg bg-surface-muted px-3 py-1.5 text-sm"
            >
              <span className="font-mono text-xs">
                {e.host}
                {e.port ? `:${e.port}` : ""}
              </span>
              <button
                type="button"
                onClick={() => onChange(egress.filter((_, j) => j !== i))}
                className="text-xs text-foreground-muted hover:text-danger"
              >
                Remove
              </button>
            </li>
          ))}
        </ul>
      )}
      <div className="flex items-center gap-2">
        <input
          value={host}
          onChange={(e) => setHost(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              add();
            }
          }}
          placeholder="host, e.g. api.github.com"
          className="flex-1 rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
        />
        <input
          value={port}
          onChange={(e) => setPort(e.target.value)}
          placeholder="443"
          inputMode="numeric"
          className="w-20 rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
        />
        <button
          type="button"
          onClick={add}
          className="rounded-lg border border-border bg-surface px-3 py-2 text-sm font-medium transition hover:bg-surface-muted"
        >
          Add
        </button>
      </div>
      {err && <p className="text-xs text-danger">{err}</p>}
    </div>
  );
}

function PackageSection({
  title,
  subtitle,
  children,
}: {
  title: string;
  subtitle: string;
  children: React.ReactNode;
}) {
  return (
    <section className="rounded-2xl border border-border bg-surface p-5 shadow-sm">
      <h2 className="text-sm font-semibold">{title}</h2>
      <p className="mt-0.5 text-xs text-foreground-muted">{subtitle}</p>
      <div className="mt-3">{children}</div>
    </section>
  );
}
