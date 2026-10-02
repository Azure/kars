// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

// kars Bridge — execution panel. The §20 launch control + honest live
// execution status. Launching asks the controller to materialize a governed
// sandbox; the panel reflects the real execution phase, including the honest
// "needs a real Foundry endpoint" caveat on a local cluster.

import { useEffect, useRef, useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import { StatusBadge } from "@/components/status-badge";
import { missionStatus } from "@/components/mission-status";
import type { TaskDetail, ValidationResult } from "@/lib/types";
import { runMissionClient } from "@/lib/run-mission-client";
import { setLaunch, validateTask } from "./launch-actions";

type Tone = "ok" | "warning" | "danger" | "muted";

// One projected status → one label + tone, shared with the page badge and the
// deploy timeline. The panel never invents a state the rest of the page denies.
function statusView(task: TaskDetail): { label: string; tone: Tone } {
  const blocked = task.result?.blocked ?? null;
  const assignmentMatchesCurrentRun =
    task.current_run_nonce == null
    || task.assignment?.task_id === task.current_run_nonce;
  const assignmentFailed =
    assignmentMatchesCurrentRun
    && task.assignment?.completed_at != null
    && task.assignment.state === "Failed";
  const failed =
    assignmentFailed || (task.result?.status === "error" && blocked == null);
  const delivered =
    !assignmentFailed
    && task.result != null
    && task.result.status !== "error"
    && blocked == null;
  const s = missionStatus(task.phase, task.execution_phase, {
    delivered,
    launched: task.launched,
    failed,
  });
  switch (s) {
    case "running":
      return { label: "Running", tone: "ok" };
    case "deploying":
      return { label: "Deploying", tone: "warning" };
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
  const router = useRouter();
  const [pending, startTransition] = useTransition();
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [preflight, setPreflight] = useState<{
    key: string; attempt: number; result: ValidationResult;
  } | null>(null);
  const validationEpoch = useRef(0);
  const validationKey = JSON.stringify([
    task.namespace, task.name, task.created_at, task.observed_generation,
    task.phase, task.envelope_digest, task.envelope, task.composition,
  ]);
  const validation = preflight?.key === validationKey && preflight.attempt === attempt
    ? preflight.result : null;
  const [launchAccepted, setLaunchAccepted] = useState<boolean | null>(null);
  const [rerunBaseline, setRerunBaseline] = useState<string | null | undefined>(undefined);
  const awaitingAssignment = Boolean(
    task.current_run_nonce
    && task.assignment?.task_id !== task.current_run_nonce,
  );
  const assignmentInFlight =
    awaitingAssignment
    || (
      task.assignment?.completed_at == null
      && (task.assignment?.state === "Assigned" || task.assignment?.state === "Running")
    );
  const rerunAccepted =
    assignmentInFlight
    || (rerunBaseline !== undefined && (task.result?.finished_at ?? null) === rerunBaseline);
  const launched = launchAccepted ?? task.launched;
  const effectiveTask = {
    ...task,
    launched,
    result: rerunAccepted ? null : task.result,
  };
  const ready = task.phase === "Ready";
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

  useEffect(() => {
    if (launchAccepted === null || launchAccepted === task.launched) return;
    const timer = window.setInterval(() => router.refresh(), 2_000);
    return () => window.clearInterval(timer);
  }, [launchAccepted, router, task.launched]);

  useEffect(() => {
    if (!rerunAccepted) return;
    const timer = window.setInterval(() => router.refresh(), 2_000);
    return () => window.clearInterval(timer);
  }, [rerunAccepted, router]);

  function toggle(launch: boolean) {
    if (launch && (!ready || checking || !validation?.ok)) return;
    setError(null);
    const epoch = validationEpoch.current;
    startTransition(async () => {
      try {
        // Recheck at the action boundary; stop never depends on preflight.
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
          setLaunchAccepted(launch);
          window.setTimeout(() => router.refresh(), 0);
        }
      } catch {
        if (launch) setPreflight({ key: validationKey, attempt, result: {
          ok: false,
          checks: [{ id: "validate_error", label: "Launch check interrupted", status: "fail",
            detail: "Recheck launch validation before retrying." }],
        } });
        setError("The launch control could not be reached. Recheck the mission status before retrying.");
      }
    });
  }

  function run() {
    setError(null);
    startTransition(async () => {
      const res = await runMissionClient(task.namespace, task.name);
      if (res.error) {
        setError(res.error);
      } else {
        setRerunBaseline(task.result?.finished_at ?? null);
        window.setTimeout(() => router.refresh(), 0);
      }
    });
  }

  const sandboxRunning = task.execution_phase === "Running";
  const running = sandboxRunning && effectiveTask.result == null;
  const view = statusView(effectiveTask);
  // Fail loud: if the mission is launched but the sandbox never reached Running
  // and nothing has been delivered, the run is stalled — say so with a reason,
  // never a silent "Idle". A common cause is a chat-gateway harness (Hermes)
  // that waits for messages instead of executing an autonomous loop.
  const stalled =
    launched &&
    !running &&
    task.result == null &&
    task.phase !== "Degraded" &&
    (task.activity?.length ?? 0) === 0;

  return (
    <section
      aria-labelledby="exec-heading"
      className="rounded-xl border border-border bg-surface p-6"
    >
      <div className="flex items-center justify-between gap-4">
        <div>
          <h2 id="exec-heading" className="text-sm font-semibold">
            Execution
          </h2>
          <p className="mt-0.5 text-sm text-foreground-muted">
            {rerunAccepted
              ? "A corrected rerun is in progress. This panel updates until a new terminal result arrives."
              : task.result?.status === "error"
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
          <p className="font-semibold">This run hasn&rsquo;t started producing work.</p>
          <p className="mt-1 leading-relaxed">
            The sandbox is up but the agent hasn&rsquo;t reached <span className="font-medium">Running</span> or
            emitted any activity yet. If this persists, the chosen harness may not execute an autonomous
            mission (for example, a chat-gateway harness like <span className="font-mono">Hermes</span> waits
            for inbound messages). Check the deploy timeline below, or re-compose with the{" "}
            <span className="font-mono">OpenClaw</span> harness for one-shot missions.
          </p>
        </div>
      )}

      {task.execution_detail && (
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
        <div className="mt-3 rounded-lg border border-border bg-surface-muted/40 px-3 py-2.5">
          <p className="text-xs font-medium text-foreground-muted">Pre-flight check</p>
          <ul className="mt-1.5 space-y-1">
            {validation.checks.map((c) => (
              <li key={c.id} className="flex items-start gap-2 text-xs">
                <span
                  className={
                    c.status === "pass"
                      ? "text-ok"
                      : c.status === "warn"
                        ? "text-warning"
                        : "text-danger"
                  }
                  aria-hidden
                >
                  {c.status === "pass" ? "✓" : c.status === "warn" ? "!" : "✕"}
                </span>
                <span>
                  <span className="font-medium">{c.label}</span>{" "}
                  <span className="text-foreground-muted">{c.detail}</span>
                </span>
              </li>
            ))}
          </ul>
          {!validation.ok && (
            <p className="mt-2 text-xs font-medium text-danger">
              This mission can&apos;t launch until the failing checks are resolved. Follow each check&apos;s
              instructions; budget enrollment may require a new draft rather than editing this one.
            </p>
          )}
        </div>
      )}

      <div className="mt-4 flex flex-wrap items-center gap-3">
        {!launched ? (
          <>
            <button
              type="button"
              disabled={!ready || pending || checking || !validation?.ok}
              onClick={() => toggle(true)}
              className="rounded-lg bg-signal px-4 py-2 text-sm font-medium text-signal-fg hover:opacity-90 disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
            >
              {pending ? "Validating / launching…" : "Launch"}
            </button>
            <button
              type="button"
              disabled={pending || checking}
              onClick={() => { setError(null); setAttempt((value) => value + 1); }}
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
            <button
              type="button"
              disabled={pending || !sandboxRunning}
              onClick={run}
              title={sandboxRunning ? "Run this mission again" : "The sandbox must be Running to re-run."}
              className="rounded-lg bg-signal px-4 py-2 text-sm font-medium text-signal-fg hover:opacity-90 disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
            >
              {pending ? "Running…" : "Run again"}
            </button>
            <button
              type="button"
              disabled={pending}
              onClick={() => toggle(false)}
              title="Tear down the running agent sandbox and free its resources. Keeps the mission and its deliverable."
              className="rounded-lg border border-border px-4 py-2 text-sm font-medium hover:bg-surface-muted disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
            >
              {pending ? "Stopping…" : "Stop sandbox"}
            </button>
          </>
        ) : (
          // Launched, first run in flight — it starts automatically, so there's no
          // manual "run" button to second-guess; just an honest status + a way out.
          <>
            <span className="inline-flex items-center gap-2 rounded-lg bg-surface-muted px-3 py-2 text-sm font-medium text-foreground-muted">
              {rerunAccepted || running ? "Running the mission…" : view.label === "Deploying" ? "Coming online…" : "Starting…"}
            </span>
            <button
              type="button"
              disabled={pending}
              onClick={() => toggle(false)}
              title="Tear down the agent sandbox. Keeps the mission; you can launch it again later."
              className="text-xs text-foreground-muted underline underline-offset-2 hover:text-foreground disabled:opacity-50"
            >
              {pending ? "Stopping…" : "Stop"}
            </button>
          </>
        )}
      </div>
    </section>
  );
}
