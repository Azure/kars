"use client";

// kars Bridge Operator Console — mission/team-run retention policy.
//
// Kars keeps mission/team-run records (deliverable, receipt, activity) after
// delivery by design — only the sandbox (live compute) auto-tears-down. Left
// unmanaged, records accumulate forever. This control mirrors Kubernetes'
// Job.spec.ttlSecondsAfterFinished: set a cluster-wide default TTL and the
// controller auto-deletes a delivered mission/team-run once it elapses. `0`
// (the default) disables auto-delete — nothing changes unless an admin opts
// in. A mission or team may still set its OWN override at creation,
// independent of this cluster-wide default.

import { useCallback, useEffect, useState } from "react";
import type { RetentionPolicy } from "@/lib/types";

const PRESETS = [
  { label: "Never (default)", seconds: 0 },
  { label: "1 hour", seconds: 3600 },
  { label: "24 hours", seconds: 86400 },
  { label: "7 days", seconds: 604800 },
  { label: "30 days", seconds: 2592000 },
];

export function RetentionPolicyPanel({ isAdmin = true }: { isAdmin?: boolean }) {
  const [data, setData] = useState<RetentionPolicy | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [customHours, setCustomHours] = useState("");

  const load = useCallback(async () => {
    try {
      const r = await fetch("/api/operator/retention-policy", { cache: "no-store" });
      if (!r.ok) throw new Error();
      setData(await r.json());
      setError(null);
    } catch {
      setError("Couldn't load the retention policy.");
    }
  }, []);

  useEffect(() => {
    const timer = window.setTimeout(() => void load(), 0);
    return () => window.clearTimeout(timer);
  }, [load]);

  const apply = useCallback(async (seconds: number) => {
    setSaving(true);
    try {
      const r = await fetch("/api/operator/retention-policy", {
        method: "PUT",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ default_ttl_seconds: seconds }),
      });
      if (!r.ok) {
        const body = await r.json().catch(() => null);
        throw new Error(body?.error?.message || "Save failed");
      }
      setData(await r.json());
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Save failed");
    } finally {
      setSaving(false);
    }
  }, []);

  if (error && !data) {
    return (
      <div className="rounded-xl border border-border bg-surface-muted/50 p-4 text-sm text-danger">
        {error}
      </div>
    );
  }
  if (!data) {
    return <div className="text-sm text-foreground-muted">Loading retention policy…</div>;
  }

  const active = data.default_ttl_seconds;

  return (
    <div className="kb-card p-5 sm:p-6">
      <div className="flex items-center justify-between">
        <h2 className="text-sm font-semibold">Retention — delivered mission/team-run cleanup</h2>
      </div>
      <p className="mt-1 text-xs text-foreground-muted">{data.summary}</p>
      {error && <p className="mt-2 text-xs text-danger">{error}</p>}
      <div className="mt-4 flex flex-wrap gap-2">
        {PRESETS.map((p) => (
          <button
            key={p.seconds}
            type="button"
            disabled={!isAdmin || saving}
            onClick={() => apply(p.seconds)}
            className={`rounded-lg border px-3 py-1.5 text-xs font-medium transition ${
              active === p.seconds
                ? "border-signal bg-signal/10 text-signal"
                : "border-border bg-surface text-foreground-muted hover:text-foreground"
            } ${!isAdmin ? "cursor-not-allowed opacity-60" : ""}`}
          >
            {p.label}
          </button>
        ))}
      </div>
      <div className="mt-3 flex items-center gap-2">
        <label className="text-xs text-foreground-muted">
          Custom (hours)
          <input
            type="number"
            min={0}
            value={customHours}
            onChange={(e) => setCustomHours(e.target.value)}
            disabled={!isAdmin || saving}
            placeholder="e.g. 72"
            className="mt-1 block w-28 rounded-lg border border-border bg-surface px-2.5 py-1.5 text-xs disabled:opacity-60"
          />
        </label>
        <button
          type="button"
          disabled={!isAdmin || saving || customHours.trim() === ""}
          onClick={() => apply(Math.max(0, Math.round(Number(customHours) * 3600)))}
          className="mt-4 rounded-lg border border-border bg-surface px-3 py-1.5 text-xs font-medium text-foreground-muted hover:text-foreground disabled:cursor-not-allowed disabled:opacity-60"
        >
          Apply
        </button>
      </div>
      {!isAdmin && (
        <p className="mt-3 text-[11px] text-foreground-muted">
          Read-only — switch to the Admin role to change the retention default.
        </p>
      )}
      <p className="mt-3 text-[11px] text-foreground-muted">
        This is the cluster-wide default. A mission or standing team may set its own
        retention when created, which takes precedence over this default. A team&rsquo;s
        principal and roster members are never auto-deleted — only individual missions
        and task-force run records are eligible.
      </p>
    </div>
  );
}
