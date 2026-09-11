"use client";

// kars Bridge — Loop Designer. The authoring surface for 2026 loop engineering:
// pick a feedback-loop pattern, state the goal + how success is measured, and it
// generates a structured, evaluation-driven objective/charter that ENCODES the
// loop. Because that text is what the harness runs and what a principal hands to
// the sub-agents it spawns, the loop reaches the harness and is inherited down
// the delegation tree. `onApply` writes the generated text into the objective
// (mission) or charter (team) field.

import { useMemo, useState } from "react";
import { Icon } from "@/components/icon";
import {
  patternsFor,
  scaffoldObjective,
  type LoopPattern,
  type LoopSurface,
} from "@/lib/loop-patterns";

export function LoopDesigner({
  surface,
  initialGoal = "",
  initialPatternId,
  initialCriteria = "",
  rationale,
  defaultOpen = false,
  applyLabel = "Use this loop",
  onApply,
}: {
  surface: LoopSurface;
  initialGoal?: string;
  /** Pre-select a pattern (e.g. the orchestrator's proposal) for review. */
  initialPatternId?: string;
  /** Pre-fill success criteria (e.g. the orchestrator's draft). */
  initialCriteria?: string;
  /** The orchestrator's one-line why-this-pattern note, shown in review mode. */
  rationale?: string;
  /** Open expanded immediately (review-step mode) vs the collapsed button. */
  defaultOpen?: boolean;
  applyLabel?: string;
  /**
   * Called with the generated objective/charter text when the user applies it.
   * The second argument carries the structured loop fields so a caller can
   * re-seed this designer (e.g. after back-navigation) with the user's edits.
   */
  onApply: (text: string, parts?: { goal: string; patternId: string; criteria: string }) => void;
}) {
  const patterns = useMemo(() => patternsFor(surface), [surface]);
  const [open, setOpen] = useState(defaultOpen);
  const [selected, setSelected] = useState<LoopPattern | null>(
    () => patterns.find((p) => p.id === initialPatternId) ?? null,
  );
  const [goal, setGoal] = useState(initialGoal);
  // Keep the loop goal in sync with the objective the operator typed in the main
  // field: `initialGoal` is the objective, and a useState initializer only runs
  // once, so without this the goal stayed empty when the objective was filled
  // AFTER the designer mounted — leaving "Use this loop" permanently disabled
  // (audit N2). Mirror the objective into the goal until the operator edits the
  // goal directly (tracked by whether it still equals the last seen objective).
  const [goalTouched, setGoalTouched] = useState(false);
  const effectiveGoal = goalTouched ? goal : initialGoal;
  const [criteria, setCriteria] = useState(initialCriteria);
  const [context, setContext] = useState("");

  const preview = useMemo(
    () =>
      selected
        ? scaffoldObjective(selected, { goal: effectiveGoal, criteria, context }, surface)
        : "",
    [selected, effectiveGoal, criteria, context, surface],
  );

  if (!open) {
    return (
      <button
        type="button"
        onClick={() => setOpen(true)}
        className="inline-flex items-center gap-1.5 rounded-lg border border-accent/40 bg-accent/[0.06] px-3 py-1.5 text-xs font-medium text-accent transition hover:bg-accent/10"
        title="Design a feedback loop (2026 loop engineering) that shapes how the agent iterates"
      >
        <span aria-hidden><Icon name="loop" size={14} /></span> Design a loop
      </button>
    );
  }

  return (
    <div className="mt-3 space-y-4 rounded-xl border border-accent/25 bg-accent/[0.03] p-4">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h3 className="text-sm font-semibold">Loop Designer</h3>
          <p className="mt-0.5 text-xs text-foreground-muted">
            Loop engineering (2026): design the feedback cycle, not a one-shot prompt. The loop is
            baked into what the harness runs — and inherited by any sub-agents.
          </p>
        </div>
        <button type="button" onClick={() => setOpen(false)} className="text-xs text-foreground-muted hover:text-foreground">
          Close
        </button>
      </div>

      {/* Orchestrator's proposal rationale (review mode). */}
      {rationale && (
        <div className="flex items-start gap-2 rounded-lg border border-accent/25 bg-accent/[0.05] p-2.5 text-xs">
          <span aria-hidden><Icon name="compass" size={14} /></span>
          <p className="text-foreground-muted"><span className="font-medium text-foreground">Orchestrator picked this loop:</span> {rationale} You can change the pattern or details below before running.</p>
        </div>
      )}

      {/* Pattern catalog */}
      <div className="grid gap-2 sm:grid-cols-2">
        {patterns.map((p) => {
          const active = selected?.id === p.id;
          return (
            <button
              key={p.id}
              type="button"
              onClick={() => setSelected(p)}
              className={`rounded-lg border p-3 text-left transition ${
                active
                  ? "border-accent/50 bg-accent/[0.08] ring-1 ring-accent/30"
                  : "border-border bg-surface hover:border-accent/40"
              }`}
            >
              <p className="flex items-center gap-1.5 text-sm font-medium">
                <Icon name={p.icon} className="text-accent" /> {p.name}
              </p>
              <p className="mt-0.5 text-[11px] text-foreground-muted">{p.tagline}</p>
              {active && (
                <>
                  <p className="mt-2 text-[11px] text-foreground-muted"><span className="font-medium text-foreground">Use when:</span> {p.whenToUse}</p>
                  <ol className="mt-1.5 space-y-0.5 text-[11px] text-foreground-muted">
                    {p.steps.map((s, i) => (
                      <li key={i}>{i + 1}. {s}</li>
                    ))}
                  </ol>
                </>
              )}
            </button>
          );
        })}
      </div>

      {selected && (
        <div className="space-y-3">
          <label className="block">
            <span className="text-xs font-medium text-foreground-muted">Goal — the outcome you want</span>
            <textarea
              value={effectiveGoal}
              onChange={(e) => {
                setGoalTouched(true);
                setGoal(e.target.value);
              }}
              rows={2}
              placeholder="e.g. Keep our API docs in sync with the OpenAPI spec and open a PR when they drift."
              className="mt-1 w-full resize-y rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
            />
          </label>
          <label className="block">
            <span className="text-xs font-medium text-foreground-muted">Success criteria — how &ldquo;done&rdquo; is judged (one per line)</span>
            <textarea
              value={criteria}
              onChange={(e) => setCriteria(e.target.value)}
              rows={3}
              placeholder={"e.g.\nEvery endpoint in the spec has matching docs\nNo doc references a removed field\nA PR is opened only when something actually changed"}
              className="mt-1 w-full resize-y rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
            />
            <span className="mt-1 block text-[11px] text-foreground-muted">
              Defining the checks first is the point — the agent evaluates against them every cycle.
            </span>
          </label>
          <details className="text-xs">
            <summary className="cursor-pointer text-foreground-muted">Add context / constraints (optional)</summary>
            <textarea
              value={context}
              onChange={(e) => setContext(e.target.value)}
              rows={2}
              className="mt-1 w-full resize-y rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
            />
          </details>

          {/* Live preview of the generated loop objective */}
          <div>
            <p className="text-xs font-medium text-foreground-muted">Generated {surface === "team" ? "charter" : "objective"} — this runs on the harness:</p>
            <pre className="mt-1 max-h-56 overflow-auto whitespace-pre-wrap rounded-lg border border-border bg-surface p-3 font-mono text-[11px] leading-relaxed text-foreground">
              {preview}
            </pre>
          </div>

          <div className="flex items-center justify-end gap-2">
            {!effectiveGoal.trim() && (
              <span className="text-[11px] text-foreground-muted">Describe the goal above first.</span>
            )}
            <button
              type="button"
              disabled={!effectiveGoal.trim()}
              onClick={() => {
                onApply(preview, { goal: effectiveGoal, patternId: selected.id, criteria });
                setOpen(false);
              }}
              className="rounded-lg bg-accent px-4 py-2 text-sm font-semibold text-white transition hover:opacity-90 disabled:opacity-50"
            >
              {applyLabel}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
