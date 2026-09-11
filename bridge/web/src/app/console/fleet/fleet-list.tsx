"use client";

// kars Bridge Operator Console — the sandbox fleet as a filterable working
// surface (not a raw dump). Search by name/namespace/parent, filter by phase,
// and see age at a glance; each card keeps the full conditions table for triage.

import { useMemo, useState } from "react";
import { Section, Badge } from "@/components/ui";
import type { Sandbox } from "@/lib/types";

type Tone = "ok" | "warn" | "danger" | "info" | "muted" | "accent";

function phaseTone(phase: string | null): Tone {
  switch (phase) {
    case "Running":
    case "Ready":
      return "ok";
    case "Degraded":
    case "Failed":
      return "danger";
    case "Pending":
    case "Launching":
      return "warn";
    default:
      return "muted";
  }
}

function conditionTone(type_: string, status: string): string {
  if (status === "Unknown") return "text-foreground-muted";
  if (/degrad|failure|failed|unavailable|pressure|error|backoff/i.test(type_)) {
    return status === "True" ? "text-danger" : "text-ok";
  }
  if (/available|ready|healthy|established/i.test(type_)) {
    return status === "True" ? "text-ok" : "text-danger";
  }
  return status === "True" ? "text-ok" : "text-foreground-muted";
}

function age(iso: string | null): string {
  if (!iso) return "";
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "";
  const s = Math.max(0, Math.floor((Date.now() - t) / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}

function Meta({ k, v }: { k: string; v: string }) {
  return (
    <div className="inline-flex items-baseline gap-1.5">
      <dt className="text-foreground-muted">{k}</dt>
      <dd className="font-mono">{v}</dd>
    </div>
  );
}

function resourceUsage(s: Sandbox): string | null {
  if (s.cpu_millicores == null && s.memory_bytes == null) return null;
  const cpu = s.cpu_millicores == null
    ? "—"
    : s.cpu_millicores >= 1000
      ? `${(s.cpu_millicores / 1000).toFixed(1)} cores`
      : `${Math.round(s.cpu_millicores)}m`;
  const memory = s.memory_bytes == null
    ? "—"
    : s.memory_bytes >= 1024 ** 3
      ? `${(s.memory_bytes / 1024 ** 3).toFixed(1)} GiB`
      : `${Math.round(s.memory_bytes / 1024 ** 2)} MiB`;
  return `${cpu} / ${memory}`;
}

const PHASES = ["All", "Running", "Degraded", "Pending", "Other"] as const;
type PhaseFilter = (typeof PHASES)[number];

function matchesPhase(s: Sandbox, f: PhaseFilter): boolean {
  if (f === "All") return true;
  const p = s.phase ?? "";
  if (f === "Running") return p === "Running" || p === "Ready";
  if (f === "Degraded") return p === "Degraded" || p === "Failed";
  if (f === "Pending") return p === "Pending" || p === "Launching";
  return !["Running", "Ready", "Degraded", "Failed", "Pending", "Launching"].includes(p);
}

export function FleetList({ sandboxes, initialQuery = "" }: { sandboxes: Sandbox[]; initialQuery?: string }) {
  const [q, setQ] = useState(initialQuery);
  const [phase, setPhase] = useState<PhaseFilter>("All");
  const [leadsOnly, setLeadsOnly] = useState(false);

  const filtered = useMemo(() => {
    const needle = q.trim().toLowerCase();
    return sandboxes.filter((s) => {
      if (!matchesPhase(s, phase)) return false;
      if (leadsOnly && s.parent) return false;
      if (!needle) return true;
      return (
        s.name.toLowerCase().includes(needle) ||
        s.namespace.toLowerCase().includes(needle) ||
        (s.parent ?? "").toLowerCase().includes(needle) ||
        (s.runtime ?? "").toLowerCase().includes(needle) ||
        (s.tool_policy ?? "").toLowerCase().includes(needle) ||
        (s.inference_policy ?? "").toLowerCase().includes(needle) ||
        (s.team ?? "").toLowerCase().includes(needle)
      );
    });
  }, [sandboxes, q, phase, leadsOnly]);

  const counts = useMemo(() => {
    const c: Record<PhaseFilter, number> = { All: sandboxes.length, Running: 0, Degraded: 0, Pending: 0, Other: 0 };
    for (const s of sandboxes) {
      (["Running", "Degraded", "Pending", "Other"] as PhaseFilter[]).forEach((f) => {
        if (matchesPhase(s, f)) c[f] += 1;
      });
    }
    return c;
  }, [sandboxes]);

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-2">
        <input
          value={q}
          onChange={(e) => setQ(e.target.value)}
          placeholder="Search name, namespace, parent, harness…"
          className="min-w-56 flex-1 rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
        />
        <div className="flex items-center gap-1">
          {PHASES.map((p) => (
            <button
              key={p}
              type="button"
              onClick={() => setPhase(p)}
              className={`rounded-lg border px-2.5 py-1.5 text-xs font-medium ${
                phase === p ? "border-signal/40 bg-signal/10 text-signal" : "border-border text-foreground-muted hover:text-foreground"
              }`}
            >
              {p} <span className="tabular-nums opacity-70">{counts[p]}</span>
            </button>
          ))}
        </div>
        <label className="inline-flex items-center gap-1.5 text-xs text-foreground-muted">
          <input type="checkbox" checked={leadsOnly} onChange={(e) => setLeadsOnly(e.target.checked)} /> Leads only
        </label>
      </div>

      {filtered.length === 0 ? (
        <p className="rounded-lg border border-dashed border-border px-4 py-6 text-center text-sm text-foreground-muted">
          No sandbox matches these filters.
        </p>
      ) : (
        <ul className="space-y-3">
          {filtered.map((s) => (
            <Section key={`${s.namespace}/${s.name}`} className="!p-5">
              <div className="flex items-start justify-between gap-4">
                <div className="min-w-0">
                  <p className="font-mono text-sm font-medium">
                    {s.name}
                    {!s.parent && <span className="ml-2 rounded-full bg-surface-muted px-1.5 py-0.5 text-[10px] font-medium text-foreground-muted">lead</span>}
                  </p>
                  <p className="mt-0.5 font-mono text-xs text-foreground-muted">{s.namespace}</p>
                </div>
                <div className="flex shrink-0 items-center gap-2">
                  {s.created && <span className="text-xs text-foreground-muted">{age(s.created)} old</span>}
                  {s.phase === "Running" && s.working === false ? (
                    <span title="The pod is Running but the agent has produced no activity — idle (e.g. a chat-gateway harness waiting for input, or between scheduled runs).">
                      <Badge tone="muted" dot>
                        Running · idle
                      </Badge>
                    </span>
                  ) : (
                    <Badge tone={phaseTone(s.phase)} dot>
                      {s.phase === "Running" && s.working ? "Running · working" : s.phase ?? "Unknown"}
                    </Badge>
                  )}
                </div>
              </div>
              <dl className="mt-3 flex flex-wrap gap-x-6 gap-y-1 text-xs">
                {s.runtime && <Meta k="harness" v={s.runtime} />}
                {s.tool_policy && <Meta k="tool policy" v={s.tool_policy} />}
                {s.inference_policy && <Meta k="inference" v={s.inference_policy} />}
                <Meta k="governance" v={s.governed ? "enabled" : "off"} />
                {s.isolation && <Meta k="isolation" v={s.isolation} />}
                {s.team && <Meta k="owner team" v={s.team} />}
                {s.parent && <Meta k="parent" v={s.parent} />}
                {s.runtime_namespace && <Meta k="runtime namespace" v={s.runtime_namespace} />}
                {resourceUsage(s) && <Meta k="CPU / memory" v={resourceUsage(s)!} />}
                {s.created && <Meta k="created" v={new Date(s.created).toLocaleString()} />}
              </dl>
              {s.message && <p className="mt-2 text-xs text-foreground-muted">{s.message}</p>}
              {s.conditions.length > 0 && (
                <table className="mt-3 w-full text-xs">
                  <tbody>
                    {s.conditions.map((c, i) => (
                      <tr key={i} className="border-t border-border">
                        <td className="py-1.5 pr-3 font-mono text-foreground-muted">{c.type_}</td>
                        <td className={`py-1.5 pr-3 font-medium ${conditionTone(c.type_, c.status)}`}>{c.status}</td>
                        <td className="py-1.5 pr-3 text-foreground-muted">{c.reason ?? ""}</td>
                        <td className="py-1.5 text-foreground-muted">{c.message ?? ""}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </Section>
          ))}
        </ul>
      )}
    </div>
  );
}
