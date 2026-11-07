// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

// Fleet activity uses partial assignment-bound observations, not accounting totals.

import { useEffect, useRef, useState } from "react";
import Link from "@/components/navigation-link";
import type { FleetTelemetry, FleetActivityItem } from "@/lib/types";

function fmtMs(ms: number | null): string {
  if (ms == null) return "";
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

export function FleetLive({ initial }: { initial: FleetTelemetry | null }) {
  const [fleet, setFleet] = useState<FleetTelemetry | null>(initial);
  const [flash, setFlash] = useState(0);
  const lastKey = useRef<string>("");

  useEffect(() => {
    let alive = true;
    let timer: ReturnType<typeof setTimeout>;
    let controller: AbortController | undefined;
    async function tick() {
      controller = new AbortController();
      const timeout = setTimeout(() => {
        controller?.abort();
        if (alive) setFleet(null);
      }, 8000);
      try {
        const res = await fetch("/api/agents/fleet", { cache: "no-store", signal: controller.signal });
        if (!res.ok) throw new Error("Activity unavailable");
        const data: FleetTelemetry = await res.json();
        if (!alive) return;
        const newest = data.feed[0];
        const key = newest ? `${newest.agent}:${newest.worker}:${newest.source}:${newest.round}:${newest.seq}` : "";
        if (key && key !== lastKey.current) {
          lastKey.current = key;
          setFlash((f) => f + 1);
        }
        setFleet(data);
      } catch {
        if (alive) setFleet(null);
      } finally {
        clearTimeout(timeout);
        if (alive) timer = setTimeout(tick, 3000);
      }
    }
    void tick();
    return () => {
      alive = false;
      controller?.abort();
      clearTimeout(timer);
    };
  }, []);

  const working = fleet?.working;
  const idle = fleet != null && working === 0;
  const unavailable = fleet == null;

  return (
    <div className="space-y-4">
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-4 lg:grid-cols-6">
        <LiveStat label="Active workers" value={working ?? "Unavailable"} accent={(working ?? 0) > 0} pulse={(working ?? 0) > 0} />
        <LiveStat label="Teams active" value={fleet?.teams_active ?? "Unavailable"} />
        <LiveStat label="Child workers" value={fleet?.sub_agents ?? "Unavailable"} />
        <LiveStat label="Observed tokens" value={fleet?.tokens_in_flight?.toLocaleString() ?? "Unavailable"} />
        <LiveStat label="Observed tool results" value={fleet?.tool_calls ?? "Unavailable"} />
        <LiveStat label="Observed model responses" value={fleet?.rounds ?? "Unavailable"} />
      </div>

      <section className="kb-card overflow-hidden">
        <div className="flex items-center justify-between border-b border-border px-5 py-3">
          <div className="flex items-center gap-2">
            <h2 className="text-sm font-semibold">Live fleet activity</h2>
            {!idle && !unavailable && (
              <span className="inline-flex items-center gap-1.5 rounded-full bg-ok/10 px-2 py-0.5 text-[11px] font-medium text-ok">
                <span className="h-1.5 w-1.5 rounded-full bg-ok kb-pulse" aria-hidden />
                updating
              </span>
            )}
          </div>
          <span className="text-[11px] text-foreground-muted">refreshes automatically · timestamped events first</span>
        </div>
        {!unavailable && <p className="px-5 py-3 text-xs text-foreground-muted">
          Partial, provisional observations from the run&apos;s verified assignment. Tool results are runtime reports,
          not independent proof of success. Counts are not complete run totals or settled spending.
          {(fleet?.activity_unavailable ?? 0) > 0 && ` Activity is unavailable for ${fleet!.activity_unavailable} active worker(s).`}
        </p>}

        {(fleet?.feed.length ?? 0) === 0 ? (
          <div className="px-5 py-10 text-center">
            {unavailable ? (
              <>
                <p className="text-sm font-medium">Live activity is unavailable</p>
                <p className="mt-1 text-xs text-foreground-muted">Kars cannot confirm current activity here. This does not mean your work has stopped. We will retry automatically; open the run for its latest status.</p>
              </>
            ) : idle ? (
              <>
                <p className="text-sm font-medium">No active worker found</p>
                <p className="mt-1 text-xs text-foreground-muted">A queued run may still be waiting for a worker. Open the run to check its status.</p>
              </>
            ) : (
              <>
                <p className="text-sm font-medium">{working} active worker{working === 1 ? "" : "s"}; no detailed observations available yet</p>
                <p className="mt-1 text-xs text-foreground-muted">Missing observations do not establish that no work has occurred. Open the run&apos;s Execution flow for details.</p>
              </>
            )}
          </div>
        ) : (
          <ul key={flash} className="kb-stagger divide-y divide-border">
            {fleet!.feed.map((e, i) => (
              <FeedRow key={`${e.agent}-${e.round}-${e.seq}-${i}`} e={e} fresh={i === 0} />
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}

function LiveStat({
  label,
  value,
  accent,
  pulse,
  hint,
}: {
  label: string;
  value: string | number;
  accent?: boolean;
  pulse?: boolean;
  hint?: string;
}) {
  return (
    <div
      className={`relative overflow-hidden rounded-xl border p-3.5 shadow-sm ${
        accent ? "border-signal/30 bg-gradient-to-br from-signal/[0.08] to-transparent" : "border-border bg-surface"
      }`}
    >
      {pulse && <span aria-hidden className="pointer-events-none absolute -right-5 -top-5 h-14 w-14 rounded-full bg-signal/15 blur-2xl" />}
      <p className={`text-xl font-semibold tabular-nums leading-none ${accent ? "text-signal" : "text-foreground"}`}>{value}</p>
      <p className="mt-1.5 text-[11px] font-medium text-foreground-muted">{label}</p>
      {hint && <p className="mt-0.5 text-[10px] text-foreground-muted/80">{hint}</p>}
    </div>
  );
}

function FeedRow({ e, fresh }: { e: FleetActivityItem; fresh: boolean }) {
  const isTool = e.kind === "tool";
  const dotCls = e.failed ? "bg-rose-500" : isTool ? "bg-signal" : "bg-foreground-muted";
  return (
    <li className={`flex items-center gap-3 px-5 py-2.5 ${fresh ? "bg-signal/[0.03]" : ""}`}>
      <span className={`h-1.5 w-1.5 shrink-0 rounded-full ${dotCls} ${fresh && !e.failed ? "kb-pulse" : ""}`} aria-hidden />
      <div className="min-w-0 flex-1">
        <div className="flex items-baseline gap-2">
          <Link href={e.team ? `/workspace/teams/${encodeURIComponent(e.team)}/runs/${encodeURIComponent(e.agent)}` : `/workspace/missions/${encodeURIComponent(e.agent)}`} className="truncate text-xs font-medium text-signal hover:underline">{e.display_name ?? e.agent}</Link>
          {e.team && <span className="shrink-0 rounded bg-surface-muted px-1.5 py-0.5 text-[10px] text-foreground-muted">{e.team}</span>}
        </div>
        <p className="truncate text-xs text-foreground-muted">
          {isTool ? (
            <>
              reported result · <span className="font-mono text-foreground">{e.label}</span>
              {e.detail ? <span className="text-foreground-muted"> · {e.detail}</span> : null}
              {e.failed ? <span className="ml-1 font-medium text-rose-500">failed</span> : null}
            </>
          ) : (
            <>
              model response <span className="text-foreground-muted">· {e.label}</span>
              {e.failed && <span className="ml-1 font-medium text-rose-500">not accepted</span>}
            </>
          )}
        </p>
        <p className="truncate text-[10px] text-foreground-muted">{e.role ?? e.worker ?? e.agent} · {e.source ?? "source unavailable"} · {e.observed_at ? new Date(e.observed_at).toLocaleString() : "timestamp unavailable"}</p>
      </div>
      <div className="shrink-0 text-right">
        <p className="text-[11px] tabular-nums text-foreground-muted">r{e.round}</p>
        {e.ms != null && <p className="text-[10px] tabular-nums text-foreground-muted/70">{fmtMs(e.ms)}</p>}
      </div>
    </li>
  );
}
