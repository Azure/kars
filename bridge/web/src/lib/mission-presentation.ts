// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { missionBlockReason, missionStatus } from "../components/mission-status";
import { missionRunState } from "./mission-run-evidence";
import { reviewedPhases, type TaskEvidenceInput } from "./team-run-evidence";
import type { Budget, TaskDetail, TaskSummary } from "./types";
import type { ReviewedProgress } from "./types/missions";

export function missionOriginalBudget(budget: Budget | null): string {
  const limits = [
    budget?.tokens != null ? `${budget.tokens.toLocaleString("en-US")} tokens` : null,
    budget?.usd_micros != null ? `${budget.usd_micros.toLocaleString("en-US")} micro-USD` : null,
  ].filter(Boolean);
  if (!limits.length) return "No explicit grant";
  return `${limits.join(" · ")}${budget?.scope === "GovernedInference" ? "" : " (unscoped)"}`;
}

type PresentationTask = TaskEvidenceInput & Pick<TaskDetail, "phase" | "status_message">;
type StatusTask = Pick<TaskSummary, "phase" | "execution_phase" | "launched" | "current_run_nonce">
  & Partial<Pick<TaskDetail, "sandbox">>;

export function missionShutdownPending(task: Pick<StatusTask, "launched" | "execution_phase" | "sandbox">): boolean {
  return task.execution_phase === "Stopping" || (!task.launched && (
    task.sandbox != null || Boolean(task.execution_phase && task.execution_phase !== "Idle")
  ));
}

function projectStatus(task: StatusTask, progress: ReviewedProgress | null | undefined,
  outcome: { delivered: boolean; failed: boolean; blocked: boolean; needsYou: boolean }) {
  const current = task.current_run_nonce && progress?.run_nonce === task.current_run_nonce ? progress : null;
  const states = current?.issue ? [] : current?.states ?? [];
  const held = states.some(state => state === "held" || state === "uncertain");
  const executionStarted = states.some(state =>
    ["active", "completed", "failed", "held", "uncertain"].includes(state));
  const failed = outcome.failed || states.includes("failed");
  let status = missionStatus(task.phase, task.execution_phase, {
    ...outcome, failed, launched: task.launched,
  });
  if (!failed && (outcome.blocked || held)) status = "blocked";
  if (!["failed", "blocked", "done", "needs_you"].includes(status)) {
    if (missionShutdownPending(task)) status = "stopping";
    else if (!task.launched && task.current_run_nonce) status = "stopped";
    else if (task.launched && current?.issue) status = "unavailable";
    else if (task.launched && states.includes("active")) status = "running";
    else if (task.launched && task.execution_phase !== "Running" && executionStarted) status = "waiting";
  }
  return { status, failed, executionStarted, held, shutdownPending: missionShutdownPending(task) };
}

/** Presentation only: child attempts never change principal readiness or authorize dispatch. */
export function missionPresentation(task: PresentationTask, needsYou = false) {
  const run = missionRunState(task);
  const phases = reviewedPhases(task) ?? [];
  const progress = task.reviewed_execution ? {
    run_nonce: task.reviewed_execution.run_nonce,
    issue: task.reviewed_execution.issue,
    states: phases.map(phase => phase.state),
  } : null;
  const rootBlock = missionBlockReason(task) ?? run.currentResult?.blocked?.detail ?? null;
  const projection = projectStatus(task, progress, {
    delivered: run.realDelivery, failed: run.failed, blocked: Boolean(rootBlock), needsYou,
  });
  const blockReason = missionBlockReason(task) ?? (projection.held
    ? "Reviewed execution is held or uncertain. Inspect the recorded phase evidence before retrying."
    : run.currentResult?.blocked?.detail ?? null);
  return { ...run, ...projection, blockReason };
}

export function missionSummaryStatus(task: TaskSummary) {
  return projectStatus(task, task.reviewed_progress, {
    delivered: task.delivered, failed: task.failed, blocked: task.blocked, needsYou: false,
  }).status;
}
