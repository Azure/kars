// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

// kars Bridge — execution panel. The §20 launch control + honest live
// execution status. Launching asks the controller to materialize a governed
// sandbox; the panel reflects the real execution phase, including the honest
// "needs a real Foundry endpoint" caveat on a local cluster.

import { useEffect, useRef, useState } from "react";
import { LiveRefresh } from "@/components/live-refresh";
import { useRouter } from "next/navigation";
import { StatusBadge } from "@/components/status-badge";
import { missionPresentation, missionShutdownPending } from "@/lib/mission-presentation";
import type { TaskDetail, ValidationResult } from "@/lib/types";
import { runMissionClient } from "@/lib/run-mission-client";
import { missionRunState } from "@/lib/mission-run-evidence";
import { ValidationChecks } from "@/components/validation-checks";
import { NewQualificationMission } from "@/components/mission-qualification";
import { setLaunch, validateTask } from "./launch-actions";

type Tone = "ok" | "warning" | "danger" | "muted";

// One projected status → one label + tone, shared with the page badge and the
// deploy timeline. The panel never invents a state the rest of the page denies.
function statusView(task: TaskDetail): { label: string; tone: Tone } {
  const { status: s } = missionPresentation(task);
  switch (s) {
    case "running":
      return { label: "Running", tone: "ok" };
    case "deploying":
      return { label: "Deploying", tone: "warning" };
    case "waiting":
      return { label: "Awaiting next phase", tone: "muted" };
    case "unavailable":
      return { label: "Execution evidence unavailable", tone: "warning" };
    case "stopping":
      return { label: "Stopping", tone: "warning" };
    case "stopped":
      return { label: "Stopped", tone: "muted" };
    case "done":
      return { label: "Delivered", tone: "ok" };
    case "blocked":
      return { label: "Blocked", tone: "danger" };
    case "failed":
      return { label: "Run failed", tone: "danger" };
    default:
      return { label: task.launched ? "Starting" : "Draft", tone: "muted" };
  }
}

export function ExecutionPanel({ task }: { task: TaskDetail }) {
  if (task.halted != null) {
    return (
      <section aria-labelledby="exec-heading" className="scroll-mt-28 rounded-xl border border-border bg-surface p-6">
        <h2 id="exec-heading" className="text-sm font-semibold">Execution permanently halted</h2>
        <p className="mt-2 text-sm text-foreground-muted">
          This Mission cannot restart. There are no launch checks to fix. You can still review its recorded
          work and download retained files. To continue, start a new Mission; the original evidence and
          accounting stay recorded.
        </p>
        <a href="/workspace/new" className="mt-3 inline-block text-sm text-signal underline">Start a new Mission</a>
      </section>
    );
  }
  return <ActiveExecutionPanel task={task} />;
}

function ActiveExecutionPanel({ task }: { task: TaskDetail }) {
  const router = useRouter();
  const [pending, setPending] = useState(false);
  const actionInFlight = useRef(false);
  const [action, setAction] = useState<"launch" | "stop" | "run" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [preflight, setPreflight] = useState<{
    key: string; attempt: number; result: ValidationResult;
  } | null>(null);
  const validationEpoch = useRef(0);
  const validationKey = JSON.stringify([
    task.namespace, task.name, task.created_at, task.observed_generation,
    task.phase, task.credential_setup_pending, task.envelope_digest, task.envelope, task.composition, task.qualification_run,
    task.run_requested, task.current_run_nonce, task.result?.assignment_nonce,
    task.result?.finished_at, task.assignment?.task_id, task.assignment?.completed_at,
  ]);
  const validation = preflight?.key === validationKey && preflight.attempt === attempt
    ? preflight.result : null;
  const [launchAccepted, setLaunchAccepted] = useState<boolean | null>(null);
  const [launchBaseline, setLaunchBaseline] = useState<number | null>(null);
  const [rerunBaseline, setRerunBaseline] = useState<string | null | undefined>(undefined);
  const { currentResult, assignmentInFlight } = missionRunState(task);
  const launchAcknowledged =
    launchAccepted !== null
    && launchAccepted === task.launched
    && task.observed_generation !== launchBaseline;
  if (launchAcknowledged) setLaunchAccepted(null);
  const launched = launchAcknowledged ? task.launched : launchAccepted ?? task.launched;
  const rerunAccepted =
    launched && task.phase !== "Degraded" && task.execution_phase !== "Degraded" && (
      assignmentInFlight
      || (rerunBaseline !== undefined && (task.result?.finished_at ?? null) === rerunBaseline)
    );
  const effectiveTask = {
    ...task,
    launched,
    result: rerunAccepted ? null : currentResult,
  };
  const shutdownPending = missionShutdownPending(effectiveTask);
  // A live assignment ending is not proof that the durable run has completed.
  const qualificationPending = !!task.qualification_run && task.run_requested
    && Boolean(task.current_run_nonce?.trim()) && currentResult == null;
  const qualificationUsed = !!task.qualification_run && (task.run_requested || currentResult != null);
  const setupPending = task.credential_setup_pending === true && !qualificationUsed;
  const ready = (task.phase === "Ready" || setupPending) && (!qualificationUsed || qualificationPending);
  const checking = !launched && validation === null;

  useEffect(() => {
    const epoch = ++validationEpoch.current;
    if (launched) return;
    let active = true;
    void validateTask(task.name).catch((): ValidationResult => ({
      ok: false,
      checks: [{
        id: "validate_error", label: "Validation could not run", status: "fail",
        detail: "Could not reach launch validation. Recheck before launching.",
      }],
    })).then((result) => {
      if (active && validationEpoch.current === epoch) {
        setPreflight({ key: validationKey, attempt, result });
      }
    });
    return () => { active = false; validationEpoch.current = epoch + 1; };
  }, [task.name, validationKey, attempt, launched]);

  async function toggle(launch: boolean) {
    if (actionInFlight.current || (launch && (shutdownPending || !ready || checking || !validation?.ok))) return;
    actionInFlight.current = true;
    setPending(true);
    setAction(launch ? "launch" : "stop");
    setError(null);
    const epoch = validationEpoch.current;
    try {
      // Recheck at the action boundary; stop never depends on preflight or page refresh.
      if (launch) {
        const result = await validateTask(task.name);
        if (validationEpoch.current !== epoch) return;
        setPreflight({ key: validationKey, attempt, result });
        if (!result.ok) return;
      } else {
        setPreflight(null);
      }
      const res = await setLaunch(task.name, launch);
      if (res.error) {
        setError(res.error);
      } else {
        setLaunchBaseline(task.observed_generation);
        setLaunchAccepted(launch);
      }
    } catch {
      if (launch) setPreflight({ key: validationKey, attempt, result: {
        ok: false,
        checks: [{ id: "validate_error", label: "Launch check interrupted", status: "fail",
          detail: "Recheck launch validation before retrying." }],
      } });
      setError("The launch control could not be reached. Recheck the mission status before retrying.");
    } finally {
      actionInFlight.current = false;
      setPending(false);
    }
  }

  async function run() {
    if (actionInFlight.current || (task.qualification_run && (!qualificationPending || !launched || !sandboxRunning))) return;
    actionInFlight.current = true;
    setPending(true);
    setAction("run");
    setError(null);
    try {
      const res = await runMissionClient(task.namespace, task.name);
      if (res.error) {
        setError(res.error);
      } else {
        setRerunBaseline(task.result?.finished_at ?? null);
      }
    } catch {
      setError("The run response could not be confirmed. Check Activity before requesting another run.");
    } finally {
      actionInFlight.current = false;
      setPending(false);
    }
  }

  const sandboxRunning = task.execution_phase === "Running";
  const view = statusView(effectiveTask);
  const completedReviewedRun = !launched && validation?.checks.some((check) =>
    check.id === "execution_activation" && check.status === "fail"
    && check.detail.startsWith("CompletedReviewedRun:"));
  const stalled =
    launched &&
    view.label === "Deploying" &&
    currentResult == null &&
    (task.activity?.length ?? 0) === 0;

  const refresh = <LiveRefresh active={shutdownPending || (launchAccepted !== null && !launchAcknowledged) || rerunAccepted} intervalMs={2_000} />;

  if (completedReviewedRun) {
    return (
      <section aria-labelledby="exec-heading" className="scroll-mt-28 rounded-xl border border-border bg-surface p-6">
        {refresh}
        <h2 id="exec-heading" className="text-sm font-semibold">Completed revision</h2>
        <p className="mt-2 text-sm text-foreground-muted">
          {task.execution_phase === "Idle" && !shutdownPending ? "The runtime is stopped." : "Sandbox shutdown was requested."}
          {" "}This reviewed run cannot be relaunched. Its output, review and accounting are retained;
          there are no launch checks to fix for this completed revision.
        </p>
        <p className="mt-2 text-sm text-foreground-muted">
          To continue, use Request changes in Deliverable, then review and launch the distinct revision
          from Overview. This does not resume the old process or replay its completed phases.
        </p>
      </section>
    );
  }

  return (
    <section
      aria-labelledby="exec-heading"
      className="scroll-mt-28 rounded-xl border border-border bg-surface p-6"
    >
      {refresh}
      <div className="flex items-center justify-between gap-4">
        <div>
          <h2 id="exec-heading" className="text-sm font-semibold">
            Execution
          </h2>
          <p className="mt-0.5 text-sm text-foreground-muted">
            {shutdownPending
              ? "Sandbox shutdown is still being confirmed. This page updates automatically; Launch remains disabled until cleanup finishes."
              : setupPending
              ? "Launch can finish this saved draft’s keyless setup. It must verify the existing empty source before starting; it cannot create or rewrite credentials."
              : view.label === "Blocked"
              ? "Execution is held. Resolve the recorded reason before retrying."
              : view.label === "Run failed"
              ? "This revision has a recorded failure. Inspect the current root and reviewed phase evidence."
              : view.label === "Awaiting next phase"
              ? "Reviewed work is recorded; the next phase or principal handoff is pending."
              : view.label === "Execution evidence unavailable"
              ? "The reviewed phase snapshot is unavailable or changed while reading. Refresh to check its current state."
              : rerunAccepted
              ? "A run is requested and awaiting a current terminal result. Check Activity for recorded progress."
              : task.qualification_run && currentResult
              ? "This qualification attempt has ended. Review its retained output; another attempt needs a new explicitly reviewed Mission."
              : currentResult?.status === "error"
              ? "The latest run failed. Its sandbox remains available for a corrected re-run."
              : launched
              ? "This task is launched — the controller materializes a governed sandbox."
              : "Review the trust envelope above, then launch to run a governed agent."}
          </p>
        </div>
        <StatusBadge tone={view.tone} label={view.label} />
      </div>

      {task.sandbox && (
        <dl className="mt-4 flex items-center justify-between border-t border-border pt-3">
          <dt className="text-sm text-foreground-muted">Sandbox</dt>
          <dd className="font-mono text-xs">{task.sandbox}</dd>
        </dl>
      )}

      {stalled && (
        <div className="mt-3 rounded-lg border border-warning/40 bg-warning/10 px-3 py-2.5 text-xs text-warning">
          <p className="font-semibold">Execution is waiting for readiness or dispatch.</p>
          <p className="mt-1 leading-relaxed">
            Controller admission alone does not prove model access or agent activity.
            Review the deployment checks and current diagnostic reason.
            Activity shows recorded work, not a prediction that either harness has started.
          </p>
        </div>
      )}

      {task.execution_detail && (!shutdownPending || task.execution_phase === "Stopping" || task.execution_phase === "Degraded") && (
        <p className="mt-3 rounded-lg border border-dashed border-border px-3 py-2.5 text-xs text-foreground-muted">
          {task.execution_detail}
        </p>
      )}

      {error && (
        <p className="mt-3 rounded-lg border border-danger/30 bg-danger/10 px-3 py-2 text-xs text-danger">
          {error}
        </p>
      )}

      {!launched && (
        <p role="status" className="mt-3 text-xs text-foreground-muted">
          {checking ? "Checking this saved package… Launch is disabled until validation completes."
            : !validation?.ok ? "Launch blocked — resolve the failing checks below."
            : validation.checks.some((check) => check.status === "warn")
              ? "Pre-flight completed with warnings — review them before requesting launch."
              : "Pre-flight checks passed. Launch rechecks the package; runtime admission still applies."}
        </p>
      )}

      {validation && !launched && (
        <div>
          <ValidationChecks checks={validation.checks} />
          {!validation.ok && (
            <p className="mt-2 text-xs font-medium text-danger">
              This mission can&apos;t launch until the failing checks are resolved. Follow each check&apos;s
              instructions; budget enrollment may require a new draft rather than editing this one.
            </p>
          )}
        </div>
      )}

      {task.qualification_run && <div className="mt-4"><NewQualificationMission /></div>}
      <div className="mt-4 flex flex-wrap items-center gap-3">
        {!launched ? (
          <>
            <button
              type="button"
              disabled={shutdownPending || !ready || pending || checking || !validation?.ok}
              onClick={() => toggle(true)}
              className="rounded-lg bg-signal px-4 py-2 text-sm font-medium text-signal-fg hover:opacity-90 disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
            >
              {pending && action === "launch" ? "Validating / launching…" : "Launch"}
            </button>
            <button
              type="button"
              disabled={pending || checking}
              onClick={() => { setError(null); setAttempt((value) => value + 1); router.refresh(); }}
              className="rounded-lg border border-border px-4 py-2 text-sm font-medium hover:bg-surface-muted disabled:opacity-50"
            >
              Recheck launch
            </button>
            {!ready && (
              <span className="text-xs text-foreground-muted">
                {task.phase === "Pending"
                  ? "The controller is admitting this package. Both admission and pre-flight checks must pass before Launch enables."
                  : "This package is not launchable; review its status and validation details."}
              </span>
            )}
            {ready && validation?.ok && (
              <span className="text-xs text-foreground-muted">
                Requests a governed sandbox and starts the mission if runtime admission succeeds.
              </span>
            )}
          </>
        ) : effectiveTask.result ? (
          // Delivered (or a stop condition) — a re-run is a deliberate choice.
          <>
            {!task.qualification_run && <button
              type="button"
              disabled={pending || !sandboxRunning}
              onClick={run}
              title={sandboxRunning ? "Run this mission again" : "The sandbox must be Running to re-run."}
              className="rounded-lg bg-signal px-4 py-2 text-sm font-medium text-signal-fg hover:opacity-90 disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
            >
              {pending && action === "run" ? "Running…" : "Run again"}
            </button>}
            <button
              type="button"
              disabled={pending}
              onClick={() => toggle(false)}
              title="Tear down the running agent sandbox and free its resources. Keeps the mission and its deliverable."
              className="rounded-lg border border-border px-4 py-2 text-sm font-medium hover:bg-surface-muted disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
            >
              {pending && action === "stop" ? "Stopping…" : "Stop sandbox"}
            </button>
          </>
        ) : (
          // A qualification retry resumes the existing durable request, never a new attempt.
          <>
            {qualificationPending && <button
              type="button"
              disabled={pending || !sandboxRunning}
              onClick={run}
              title="Resume or await the existing durable request. Admission expiry does not cancel admitted work; the server decides whether it can continue."
              className="rounded-lg border border-border px-4 py-2 text-sm font-medium hover:bg-surface-muted disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
            >
              {pending && action === "run" ? "Resuming / awaiting…" : "Resume / await run"}
            </button>}
            <span className="inline-flex items-center gap-2 rounded-lg bg-surface-muted px-3 py-2 text-sm font-medium text-foreground-muted">
              {view.label === "Blocked" ? "Execution blocked"
                : view.label === "Run failed" ? "Run failed"
                : view.label === "Running" ? "Running the mission…"
                : view.label === "Deploying" ? "Coming online…" : "Awaiting execution…"}
            </span>
            <button
              type="button"
              disabled={pending}
              onClick={() => toggle(false)}
              title={task.qualification_run ? "Tear down the agent sandbox. A new qualification attempt requires a new Mission." : "Tear down the agent sandbox. Keeps the Mission and recorded evidence, not a process checkpoint. Continuation depends on the retained run state."}
              className="text-xs text-foreground-muted underline underline-offset-2 hover:text-foreground disabled:opacity-50"
            >
              {pending && action === "stop" ? "Stopping…" : "Stop"}
            </button>
          </>
        )}
      </div>
    </section>
  );
}
