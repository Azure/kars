// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

// kars Bridge Workspace — a team's live "watching" status. Shows a calm,
// continuously-updating countdown to the next charter tick so the operator can
// see, at a glance, that the team is actively on watch (not stalled).

import { useEffect, useState } from "react";
import type { TeamLifecycleMode, TeamRuntimeState } from "@/lib/types";
import { formatWarmIdle } from "@/lib/format";

function fmtDelta(ms: number): string {
  if (ms <= 0) return "any moment now";
  const s = Math.round(ms / 1000);
  if (s < 60) return `in ${s}s`;
  const m = Math.floor(s / 60);
  const rem = s % 60;
  if (m < 60) return rem ? `in ${m}m ${rem}s` : `in ${m}m`;
  const h = Math.floor(m / 60);
  return `in ${h}h ${m % 60}m`;
}

function fmtAgo(ms: number): string {
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m ago`;
}

export function WatchingStatus({
  nextRunAt,
  lastRunAt,
  everyMinutes,
  lifecycleMode,
  warmIdleSeconds,
  runtimeState,
  runInFlight,
  runRunning,
  runHeld = false,
  currentAssignment,
  idleDeadlineAt,
  memoryEntries,
  paused,
  health,
}: {
  nextRunAt: string | null;
  lastRunAt: string | null;
  everyMinutes: number | null;
  lifecycleMode: TeamLifecycleMode;
  warmIdleSeconds: number | null;
  runtimeState: TeamRuntimeState | null;
  runInFlight: boolean;
  runRunning: boolean;
  runHeld?: boolean;
  currentAssignment: string | null;
  idleDeadlineAt: string | null;
  memoryEntries: number;
  paused: boolean;
  health?: string | null;
}) {
  const [now, setNow] = useState<number | null>(null);
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, []);

  if (paused) {
    const detail = "No new runs will start until you resume. Current work can finish and still uses tokens. Runtime cleanup follows the team's existing retention settings; Pause does not freeze or checkpoint a process.";
    return (
      <div className="rounded-xl border border-border bg-surface-muted p-5">
        <p className="text-sm font-medium">{runInFlight ? "Scheduling paused — current run is finishing" : "Scheduling paused"}</p>
        <p className="mt-1 text-xs text-foreground-muted">{detail}</p>
      </div>
    );
  }

  const next = nextRunAt ? new Date(nextRunAt).getTime() : null;
  const last = lastRunAt ? new Date(lastRunAt).getTime() : null;
  const idleDeadline = idleDeadlineAt ? new Date(idleDeadlineAt).getTime() : null;

  const working = runRunning || runtimeState === "Working";
  const awaitingReview = health === "AwaitingReview";
  const tone = runHeld
    ? {
        box: "border-amber-500/30 bg-amber-500/5",
        dot: "bg-amber-500",
        label: "Run held",
        note: "Reviewed execution is held. Completed role evidence remains available in the latest run; synthesis is prohibited.",
      }
    : awaitingReview
    ? {
        box: "border-amber-500/30 bg-amber-500/5",
        dot: "bg-amber-500",
        label: "Awaiting your review",
        note: "The retained outcome needs your decision before dependent work or memory promotion can proceed.",
      }
    : working
    ? {
        box: "border-sky-500/30 bg-sky-500/5",
        dot: "bg-sky-500",
        label: "Working",
        note: currentAssignment ? `Current assignment: ${currentAssignment}` : "An assignment is active.",
      }
    : runInFlight
      ? {
          box: "border-amber-500/30 bg-amber-500/5",
          dot: "bg-amber-500",
          label: "Run in progress",
          note: "Execution is starting or recovering. Follow the latest run for phase-level progress.",
        }
    : runtimeState === "Warm"
      ? {
          box: "border-emerald-500/30 bg-emerald-500/5",
          dot: "bg-emerald-500",
          label: lifecycleMode === "persistent" ? "Warm · persistent" : "Warm",
          note:
            idleDeadline != null && now != null
              ? `Hibernates ${fmtDelta(idleDeadline - now)} unless new work arrives.`
              : "Ready for the next assignment.",
        }
      : runtimeState === "Hibernating"
        ? {
            box: "border-border bg-surface-muted",
            dot: "bg-foreground-muted",
            label: "Hibernating",
            note: "No runtime compute is active. Eligible work resumes the retained team identity.",
          }
        : health === "Stalled"
      ? {
          box: "border-rose-500/30 bg-rose-500/5",
          dot: "bg-rose-500",
          label: "On watch — recent runs failing",
          note: "The last runs errored or timed out. It keeps its schedule; check the runs below.",
        }
      : health === "Unproductive"
        ? {
            box: "border-amber-500/30 bg-amber-500/5",
            dot: "bg-amber-500",
            label: "On watch — little new output",
            note: "Recent runs completed but produced little new material for the commons.",
          }
        : {
            box: "border-emerald-500/30 bg-emerald-500/5",
            dot: "bg-emerald-500",
            label: "Idle · ready",
            note: null as string | null,
          };

  return (
    <div className={`rounded-xl border p-5 ${tone.box}`}>
      <div className="flex items-center gap-2">
        <span className="relative flex h-2.5 w-2.5">
          <span className={`absolute inline-flex h-full w-full animate-ping rounded-full opacity-70 ${tone.dot}`} />
          <span className={`relative inline-flex h-2.5 w-2.5 rounded-full ${tone.dot}`} />
        </span>
        <p className="text-sm font-medium">{tone.label}</p>
      </div>
      {tone.note && <p className="mt-1 text-xs text-foreground-muted">{tone.note}</p>}
      <p className="mt-2 text-sm">
        {runHeld ? (
          "No final delivery recorded for this held run"
        ) : awaitingReview ? (
          "Schedule waits for your decision"
        ) : runInFlight || working ? (
          "Current assignment in progress"
        ) : next != null ? (
          <>
            Next check{" "}
            <span className="font-medium">
              {now == null ? "scheduled" : fmtDelta(next - now)}
            </span>
          </>
        ) : (
          "Standing by"
        )}
        {everyMinutes != null && (
          <span className="text-foreground-muted"> · every {everyMinutes} min</span>
        )}
      </p>
      {last != null && (
        <p className="mt-1 text-xs text-foreground-muted">
          Last check {now == null ? "recorded" : fmtAgo(now - last)}
        </p>
      )}
      <p className="mt-1 text-xs text-foreground-muted">
        {memoryEntries} approved memor{memoryEntries === 1 ? "y" : "ies"} available to the next assignment
      </p>
      <p className="mt-2 text-[11px] text-foreground-muted">
        {lifecycleMode === "persistent"
          ? "Persistent runtime · remains ready until explicitly paused"
          : lifecycleMode === "resourceOptimized"
            ? `Resource optimized · suspends ${formatWarmIdle(warmIdleSeconds) === "immediately" ? "immediately when idle" : `after ${formatWarmIdle(warmIdleSeconds)} idle`}`
            : "Ephemeral runtime · a clean sandbox is created for each assignment"}
      </p>
    </div>
  );
}
