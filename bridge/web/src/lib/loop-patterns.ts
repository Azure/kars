// kars Bridge — Loop engineering catalog (2026).
//
// "Loop engineering" is the 2026 discipline that supersedes one-shot prompt
// engineering: you design the feedback-driven cycle an agent runs (observe →
// reason → act → evaluate → repeat), define how success is MEASURED first
// (evaluation-driven authoring), and let the loop — not a static prompt — carry
// the work. In kars a loop is not a separate resource: it is the STRUCTURE of the
// objective (a mission) or charter (a team) the harness actually runs. Because
// the objective/charter is what the runtime executes and what a principal hands
// to the sub-agents it spawns, a loop authored here reaches the harness AND is
// inherited down the delegation tree — every pattern's scaffold ends with an
// explicit sub-agent-inheritance clause so the loop propagates, not just the goal.

export type LoopSurface = "mission" | "team";

export interface LoopPattern {
  id: string;
  name: string;
  /** One-line essence. */
  tagline: string;
  /** When this loop is the right choice. */
  whenToUse: string;
  /** The cycle steps, shown as the loop's shape. */
  steps: string[];
  /** Which surfaces this pattern suits (mission = single run, team = standing). */
  surfaces: LoopSurface[];
  /** Icon name (see components/icon.tsx) for the card. */
  icon: import("@/components/icon").IconName;
}

export interface LoopInputs {
  /** The outcome the user wants. */
  goal: string;
  /** How success is judged — the evaluation criteria (may be blank). */
  criteria: string;
  /** Optional extra context / constraints. */
  context?: string;
}

/** The 2026 loop-engineering catalog. Ordered from most common to specialised. */
export const LOOP_PATTERNS: LoopPattern[] = [
  {
    id: "react",
    name: "ReAct — Reason + Act",
    tagline: "Think, use a tool, observe, repeat — the workhorse for tool-using work.",
    whenToUse:
      "Research, investigation, or anything that needs tools/web/repos where each step depends on what the last one returned.",
    steps: ["Reason about the next step", "Act (call a tool)", "Observe the result", "Repeat until the goal is met"],
    surfaces: ["mission", "team"],
    icon: "loop",
  },
  {
    id: "reflect",
    name: "Reflect-Refine — self-critique",
    tagline: "Draft, critique your own draft against the bar, revise — until it clears.",
    whenToUse: "Quality-critical output: reports, code, analysis where the first pass is rarely good enough.",
    steps: ["Produce a draft", "Critique it against the success criteria", "Revise the weak parts", "Repeat until it passes"],
    surfaces: ["mission", "team"],
    icon: "mirror",
  },
  {
    id: "plan-execute",
    name: "Plan-Execute — plan then do",
    tagline: "Make a concrete plan, execute step by step, re-plan when a step fails.",
    whenToUse: "Multi-step tasks with clear sub-goals — migrations, build-outs, structured deliverables.",
    steps: ["Write a concrete step-by-step plan", "Execute the next step", "Check the outcome", "Re-plan on failure; continue on success"],
    surfaces: ["mission", "team"],
    icon: "map",
  },
  {
    id: "eval-iterate",
    name: "Evaluate-Iterate — tests first",
    tagline: "Define acceptance checks up front, then loop until every check passes.",
    whenToUse:
      "When 'done' must be objective and verifiable. The evaluation-driven pattern — write the checks before the work.",
    steps: ["Turn the goal into explicit acceptance checks", "Attempt the work", "Run each check", "Fix failures and re-check until all pass"],
    surfaces: ["mission", "team"],
    icon: "check-cycle",
  },
  {
    id: "explore-branch",
    name: "Explore-Branch — tree of thoughts",
    tagline: "Generate several candidate approaches, evaluate, keep the best, prune the rest.",
    whenToUse: "Hard or open-ended problems where the first idea is unlikely to be the best.",
    steps: ["Generate 2–3 distinct candidate approaches", "Evaluate each against the criteria", "Prune the weak ones", "Deepen the best; repeat"],
    surfaces: ["mission"],
    icon: "branch",
  },
  {
    id: "standing-watch",
    name: "Standing Watch — cadence loop",
    tagline: "Periodically observe, detect what changed since last time, act, report — with memory.",
    whenToUse: "Standing teams that monitor something over time (a repo, a market, a system) and act on change.",
    steps: ["Observe the current state", "Compare with memory of last run", "Act only on meaningful change", "Report and record what you learned"],
    surfaces: ["team"],
    icon: "eye",
  },
];

export function patternsFor(surface: LoopSurface): LoopPattern[] {
  return LOOP_PATTERNS.filter((p) => p.surfaces.includes(surface));
}

/**
 * Scaffold a structured, evaluation-driven objective/charter that ENCODES the
 * loop — so the harness runs the loop, not a bare instruction. The output always
 * carries: the goal, the loop cycle, explicit success criteria, a stop
 * condition, and a sub-agent-inheritance clause so any spawned sub-agent runs
 * the same loop. `surface` tunes the wording (a mission runs once to a
 * deliverable; a team runs the loop on every cadence tick).
 */
export function scaffoldObjective(
  pattern: LoopPattern,
  inputs: LoopInputs,
  surface: LoopSurface,
): string {
  const goal = inputs.goal.trim() || "<describe the outcome you want>";
  const criteria = inputs.criteria.trim();
  const context = (inputs.context ?? "").trim();

  const cycle = pattern.steps.map((s, i) => `  ${i + 1}. ${s}.`).join("\n");

  const criteriaBlock = criteria
    ? `SUCCESS CRITERIA (how this is judged — evaluate against these every cycle):\n${criteria
        .split(/\n|;/)
        .map((c) => c.trim())
        .filter(Boolean)
        .map((c) => `  • ${c}`)
        .join("\n")}`
    : `SUCCESS CRITERIA: State the checks you'll judge yourself against before you start, then evaluate against them every cycle.`;

  const stop =
    surface === "team"
      ? "STOP CONDITION: End the run once the success criteria are met for this cycle; if nothing meaningful changed since last run, say so briefly and stop (don't invent work)."
      : "STOP CONDITION: Stop as soon as the success criteria are all met — don't loop further once you're done.";

  const inheritance =
    "SUB-AGENT INHERITANCE: If you delegate to sub-agents, give EACH the same loop — the cycle above and these success criteria — so the whole tree works the same way, not just you.";

  const contextBlock = context ? `\nCONTEXT / CONSTRAINTS:\n${context}\n` : "";

  const header =
    surface === "team"
      ? `LOOP: ${pattern.name} (run this loop on every cadence tick).`
      : `LOOP: ${pattern.name}.`;

  return [
    header,
    ``,
    `GOAL: ${goal}`,
    contextBlock ? contextBlock.trimEnd() : ``,
    `CYCLE — repeat until done:`,
    cycle,
    ``,
    criteriaBlock,
    ``,
    stop,
    inheritance,
  ]
    .filter((l) => l !== null && l !== undefined)
    .join("\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}
