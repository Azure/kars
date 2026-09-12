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
import { useActionState } from "react";
import { OrchestrationCube } from "@/components/orchestration-cube";
import { Icon } from "@/components/icon";
import { JourneyRail } from "@/components/journey-rail";
import { LoopDesigner } from "@/components/loop-designer";
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

import { modelKey } from "./intake-flow/helpers";
import { renderReview } from "./intake-flow/review";

const EXAMPLES = [
  "Audit our README for outdated install steps and propose fixes.",
  "Draft a competitive teardown of the top 3 agent platforms.",
  "Summarize this contract's risk and obligations.",
];

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
  return renderReview({
    formAction,
    objective,
    tier,
    budgetTokens,
    launch,
    blueprint,
    delegation,
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
    setEgressMode,
    egressMode,
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
  });
}
