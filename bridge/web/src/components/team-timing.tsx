"use client";

import { useEffect, useState } from "react";

function distance(ms: number): string {
  const seconds = Math.max(0, Math.floor(Math.abs(ms) / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ${minutes % 60}m`;
  return `${Math.floor(hours / 24)}d ${hours % 24}h`;
}

export function TeamTiming({
  lastActivityAt,
  nextActivityAt,
  paused = false,
  compact = false,
}: {
  lastActivityAt: string | null;
  nextActivityAt: string | null;
  paused?: boolean;
  compact?: boolean;
}) {
  const [now, setNow] = useState<number | null>(null);
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);

  const last = lastActivityAt ? new Date(lastActivityAt).getTime() : null;
  const next = nextActivityAt ? new Date(nextActivityAt).getTime() : null;
  const className = compact
    ? "mt-2 flex flex-wrap gap-x-3 gap-y-1 text-[11px] text-foreground-muted"
    : "flex flex-wrap gap-x-5 gap-y-1 rounded-xl border border-border bg-surface px-4 py-3 text-xs text-foreground-muted";

  return (
    <div className={className}>
      <span title={lastActivityAt ?? undefined}>
        Last activity: {last == null ? "none yet" : now == null ? "recorded" : `${distance(now - last)} ago`}
      </span>
      <span title={nextActivityAt ?? undefined}>
        Next scheduled activity:{" "}
        {paused
          ? "paused"
          : next == null
          ? "on demand"
          : now == null
            ? "scheduled"
          : next <= now
            ? "due now"
            : `in ${distance(next - now)}`}
      </span>
    </div>
  );
}
