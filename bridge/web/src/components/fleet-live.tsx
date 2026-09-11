"use client";

// kars Bridge Workspace — fleet live telemetry. The "at scale" view: instead of
// drilling into one mission, watch the WHOLE fleet's live tool-by-tool work in
// one stream, with aggregate live metrics that tick as agents work. Polls the
// fleet endpoint on a fast cadence; everything shown is real trace data — an
// idle fleet shows an honest calm state, never fabricated ticks.

import { useEffect, useRef, useState } from "react";
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
    async function tick() {
      try {
        const res = await fetch("/api/agents/fleet", { cache: "no-store" });
        if (!res.ok) return;
        const data: FleetTelemetry = await res.json();
        if (!alive) return;
        // Flash the feed when the newest event changed (a fresh action landed).
        const key = data.feed[0] ? `${data.feed[0].agent}:${data.feed[0].round}:${data.feed[0].seq}` : "";
        if (key && key !== lastKey.current) {
          lastKey.current = key;
          setFlash((f) => f + 1);
        }
        setFleet(data);
      } catch {
        /* transient — keep last good */
      }
    }
    const id = setInterval(tick, 3000);
    void tick();
    return () => {
      alive = false;
      clearInterval(id);
    };
  }, []);

  const working = fleet?.working ?? 0;
  const idle = working === 0;
  // Distinguish genuinely-working from just-booted: sandboxes are up but no
  // model round/tool call has landed yet. Prevents "Working now 4" reading as
  // broken next to "Model rounds 0" — it's warmup, and we say so.
  const warming = working > 0 && (fleet?.rounds ?? 0) === 0 && (fleet?.tool_calls ?? 0) === 0;

  return (
    <div className="space-y-4">
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-4 lg:grid-cols-6">
        <LiveStat label={warming ? "Starting up" : "Working now"} value={working} accent={working > 0} pulse={working > 0} hint={warming ? "sandboxes up — no model round yet" : undefined} />
        <LiveStat label="Teams active" value={fleet?.teams_active ?? 0} />
        <LiveStat label="Sub-agents" value={fleet?.sub_agents ?? 0} />
        <LiveStat label="Tokens in flight" value={(fleet?.tokens_in_flight ?? 0).toLocaleString()} />
        <LiveStat label="Tool calls" value={fleet?.tool_calls ?? 0} />
        <LiveStat label="Model rounds" value={fleet?.rounds ?? 0} />
      </div>

      <section className="kb-card overflow-hidden">
        <div className="flex items-center justify-between border-b border-border px-5 py-3">
          <div className="flex items-center gap-2">
            <h2 className="text-sm font-semibold">Live fleet activity</h2>
            {!idle && (
              <span className="inline-flex items-center gap-1.5 rounded-full bg-ok/10 px-2 py-0.5 text-[11px] font-medium text-ok">
                <span className="h-1.5 w-1.5 rounded-full bg-ok kb-pulse" aria-hidden />
                streaming
              </span>
            )}
          </div>
          <span className="text-[11px] text-foreground-muted">every 3s · newest first</span>
        </div>

        {(fleet?.feed.length ?? 0) === 0 ? (
          <div className="px-5 py-10 text-center">
            {idle ? (
              <>
                <p className="text-sm font-medium">The fleet is calm</p>
                <p className="mt-1 text-xs text-foreground-muted">
                  No agent is working this moment. When a mission or standing team runs, every tool
                  call and model round streams here live — across the whole fleet.
                </p>
              </>
            ) : (
              <>
                <p className="text-sm font-medium">
                  {working} agent{working === 1 ? "" : "s"} warming up…
                </p>
                <p className="mt-1 text-xs text-foreground-muted">
                  The sandbox is up; the first model rounds and tool calls will stream here the moment
                  they happen.
                </p>
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
          <span className="truncate text-xs font-medium">{e.display_name ?? e.agent}</span>
          {e.team && <span className="shrink-0 rounded bg-surface-muted px-1.5 py-0.5 text-[10px] text-foreground-muted">{e.team}</span>}
        </div>
        <p className="truncate text-xs text-foreground-muted">
          {isTool ? (
            <>
              called <span className="font-mono text-foreground">{e.label}</span>
              {e.detail ? <span className="text-foreground-muted"> · {e.detail}</span> : null}
              {e.failed ? <span className="ml-1 font-medium text-rose-500">failed</span> : null}
            </>
          ) : (
            <>
              model round <span className="text-foreground-muted">· {e.label}</span>
            </>
          )}
        </p>
      </div>
      <div className="shrink-0 text-right">
        <p className="text-[11px] tabular-nums text-foreground-muted">r{e.round}</p>
        {e.ms != null && <p className="text-[10px] tabular-nums text-foreground-muted/70">{fmtMs(e.ms)}</p>}
      </div>
    </li>
  );
}
