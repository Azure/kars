// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

import type { TaskDetail } from "@/lib/types";
import { useLiveTrace } from "@/components/use-live-trace";
import { activityForRevision } from "@/lib/mission-run-evidence";
import { missionPresentation } from "@/lib/mission-presentation";
import { displayedPhaseRouterActivity, reviewedPhases } from "@/lib/team-run-evidence";
import { liveRouterActivity, reviewedPhaseKey, type LiveRouterActivity } from "@/lib/router-activity";
import { formatEvidenceTime } from "@/lib/format";

type StepState = "done" | "active" | "pending" | "failed";

function dotClass(s: StepState): string {
  return s === "done"
    ? "bg-emerald-500"
    : s === "failed"
      ? "bg-danger"
    : s === "active"
      ? "bg-signal kb-pulse"
      : "bg-surface-muted";
}

export function LiveDeployTimeline({ task, observing }: { task: TaskDetail; observing: boolean }) {
  const { events, routerActivity, phaseActivity } = useLiveTrace(
    task.namespace, task.name, observing, task.activity, task.current_run_nonce,
  );
  return <DeployTimeline task={{ ...task, activity: events }} liveActivity={routerActivity} phaseActivity={phaseActivity} />;
}

export function DeployTimeline({ task, liveActivity = null, phaseActivity: livePhases = {} }: {
  task: TaskDetail;
  liveActivity?: LiveRouterActivity | null;
  phaseActivity?: Record<string, LiveRouterActivity>;
}) {
  const phase = task.execution_phase;
  const running = phase === "Running";
  const launching = phase === "Launching" || phase === "Pending";
  const hasSandbox = !!task.sandbox;
  const lastSeen = formatEvidenceTime(task.agent_identity?.last_seen);
  const identityObserved = lastSeen !== "Time unavailable";
  const { failed, realDelivery: succeeded, status, executionStarted, routerActivity } = missionPresentation(task);
  const observing = task.launched && !failed && !succeeded && status !== "stopped" && status !== "stopping";
  const live = observing && task.current_run_nonce
    ? liveRouterActivity(liveActivity, task.current_run_nonce) : null;
  const observations = routerActivity ?? live?.observations;
  const principalActivity = (task.activity ?? []).some(
    event => activityForRevision(event, task.current_run_nonce ?? ""),
  ) || (observations?.state === "observed" && observations.trace.events.length > 0);
  const phaseActivity = (reviewedPhases(task) ?? []).filter(phase => {
    const observations = displayedPhaseRouterActivity({
      ...phase, live_router_activity: observing ? livePhases[reviewedPhaseKey(phase)] ?? null : null,
    })?.observations;
    return observations?.state === "observed" && observations.trace.events.length > 0;
  }).length;
  const firstActivity = principalActivity || phaseActivity > 0;
  const held = status === "blocked" || status === "stopping" || status === "stopped" || status === "unavailable";

  const steps: { label: string; detail: string; state: StepState }[] = [
    {
      label: "Launch approved",
      detail: "You authorized the start — the controller began materializing a sandbox.",
      state: task.launched ? "done" : "pending",
    },
    {
      label: "Principal sandbox reference",
      detail: hasSandbox
        ? `Controller recorded sandbox ${task.sandbox}. This reference does not establish pod readiness.`
        : phaseActivity
        ? "Reviewed phases have recorded work; the principal sandbox can start after their handoffs are ready."
        : "Waiting for the controller to record the sandbox.",
      state: hasSandbox ? "done" : launching ? "active" : "pending",
    },
    {
      label: "Controller execution status",
      detail: `Controller reports ${phase ?? "no execution status"}. Pod health and individual access checks are not included in this record.`,
      state: failed ? "failed" : running || succeeded ? "done" : hasSandbox ? "active" : "pending",
    },
    {
      label: "Mesh registry observation",
      detail: identityObserved
        ? `Registry last saw this identity at ${lastSeen}. This is not a current reachability check or revision-producer proof.`
        : "No timestamped mesh registry observation is available yet.",
      state: identityObserved ? "done" : running ? "active" : "pending",
    },
    {
      label: "Working",
      detail: failed
        ? "This revision ended without an eligible deliverable. Open Run failed for the recorded reason."
        : succeeded
        ? "This revision has an eligible recorded deliverable. Review its output and evidence."
        : principalActivity
        ? "Timestamped activity is recorded for this revision. Inspect Activity for the reported outcomes."
        : phaseActivity
        ? `Revision-bound router activity is retained for ${phaseActivity} reviewed ${phaseActivity === 1 ? "phase" : "phases"}. Inspect Activity for their reported outcomes; this does not establish principal readiness.`
        : "Waiting for the first revision-bound model round or tool record.",
      state: failed ? "failed" : succeeded ? "done" : !held && status === "running" ? "active" : "pending",
    },
  ];

  const doneCount = steps.filter((s) => s.state === "done").length;
  const workObserved = succeeded || firstActivity;

  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">{failed ? "Run failed" : status === "blocked" ? "Execution held" : status === "unavailable" ? "Execution evidence unavailable" : status === "stopping" ? "Stopping" : status === "stopped" ? "Stopped" : succeeded ? "Delivered" : status === "waiting" ? "Awaiting next phase" : firstActivity || (executionStarted && status === "running") ? "Execution observed" : "Starting execution"}</h2>
          <p className="mt-0.5 text-xs text-foreground-muted">
            {failed
              ? "The recorded outcome is unsuccessful; it does not establish which provisioning checks passed."
              : "Controller status, registry observations, and revision activity are separate signals — not a combined verification of readiness or access."}
          </p>
        </div>
        <span
          className={`shrink-0 rounded-full border px-2.5 py-1 text-xs font-medium ${
            failed
              ? "border-danger/30 bg-danger/10 text-danger"
              : workObserved
              ? "border-emerald-500/30 bg-emerald-500/10 text-emerald-600"
              : "border-signal/30 bg-signal/10 text-signal"
          }`}
        >
          {failed ? "Failed" : succeeded ? "Delivered" : `${doneCount}/${steps.length} signals`}
        </span>
      </div>
      <ol className="mt-4 space-y-0">
        {steps.map((s, i) => (
          <li key={s.label} className="flex gap-3">
            <div className="flex flex-col items-center">
              <span className={`mt-1 h-2.5 w-2.5 shrink-0 rounded-full ${dotClass(s.state)}`} aria-hidden />
              {i < steps.length - 1 && (
                <span className={`my-0.5 w-px flex-1 ${s.state === "done" ? "bg-emerald-500/40" : "bg-border"}`} aria-hidden />
              )}
            </div>
            <div className={`pb-4 ${s.state === "pending" ? "opacity-60" : ""}`}>
              <p className="text-sm font-medium leading-tight">
                {s.label}
                {s.state === "active" && (
                  <span className="ml-2 align-middle text-[10px] font-normal text-signal">in progress</span>
                )}
              </p>
              <p className="mt-0.5 text-xs text-foreground-muted">{s.detail}</p>
            </div>
          </li>
        ))}
      </ol>
    </section>
  );
}
