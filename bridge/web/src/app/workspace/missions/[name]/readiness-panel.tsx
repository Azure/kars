// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Materialized configuration is not evidence of successful access checks.

import type { Composition } from "@/lib/types";
import { egressScope, humanizeMcp } from "@/lib/format";

type Readiness = "configured" | "pending" | "attention";

function StatusDot({ state }: { state: Readiness }) {
  const map: Record<Readiness, { cls: string; glyph: string; label: string }> = {
    configured: { cls: "text-foreground-muted", glyph: "○", label: "Configured" },
    pending: { cls: "text-foreground-muted", glyph: "○", label: "Retained" },
    attention: { cls: "text-amber-600", glyph: "!", label: "Attention" },
  };
  const m = map[state];
  return (
    <span className={`inline-flex items-center gap-1.5 text-xs font-medium ${m.cls}`}>
      <span
        aria-hidden
        className={`grid h-4 w-4 place-items-center rounded-full text-[10px] ${state === "attention" ? "bg-amber-500/10" : "bg-surface-muted"}`}
      >
        {m.glyph}
      </span>
      {m.label}
    </span>
  );
}

function CheckRow({
  label,
  value,
  state,
}: {
  label: string;
  value: string;
  state: Readiness;
}) {
  return (
    <li className="flex items-center justify-between gap-4 px-4 py-2.5">
      <div className="min-w-0">
        <p className="text-sm font-medium">{label}</p>
        <p className="truncate text-xs text-foreground-muted">{value}</p>
      </div>
      <StatusDot state={state} />
    </li>
  );
}

export function ReadinessPanel({
  composition,
  launched,
  executionPhase,
  degraded,
  delivered,
  failed,
}: {
  composition: Composition;
  launched: boolean;
  executionPhase: string | null;
  degraded: boolean;
  delivered?: boolean;
  failed?: boolean;
}) {
  const live =
    launched && (executionPhase === "Running" || executionPhase === "Ready" || executionPhase === "Succeeded");
  const base: Readiness = degraded ? "attention" : live ? "configured" : "pending";

  const rows: { label: string; value: string }[] = [
    { label: "Model path", value: composition.model ?? "controller default" },
    { label: "Isolation boundary", value: composition.isolation ?? "standard" },
  ];
  if (composition.tool_policy) rows.push({ label: "Tool policy", value: composition.tool_policy });
  if (composition.mcp_servers.length)
    rows.push({ label: "Connected services", value: composition.mcp_servers.map(humanizeMcp).join(", ") });
  rows.push({
    label: "Network egress",
    value: composition.egress.length
      ? (() => {
          const ext = composition.egress.filter((e) => egressScope(e) === "external");
          const int = composition.egress.filter((e) => egressScope(e) === "internal");
          const parts: string[] = [];
          if (ext.length) parts.push(`${ext.length} external (${ext.join(", ")})`);
          if (int.length) parts.push(`${int.length} internal (${int.join(", ")})`);
          return parts.join(" · ");
        })()
      : "No additional direct egress configured",
  });
  if (composition.memory) rows.push({ label: "Shared memory", value: composition.memory });

  const headline = degraded
    ? "The controller reports degraded execution; inspect diagnostics for the cause"
    : failed
      ? "Execution failed; this configuration does not establish which accesses succeeded"
      : delivered
        ? "Retained configuration — delivery does not verify every configured access"
        : "Configured routes and policies — individual access checks are not recorded here";

  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">Access configuration</h2>
          <p className="mt-0.5 text-xs text-foreground-muted">{headline}</p>
        </div>
        <span
          className={`shrink-0 rounded-full border px-2.5 py-1 text-xs font-medium ${
            degraded
                ? "border-amber-500/30 bg-amber-500/10 text-amber-600"
                : "border-border bg-surface-muted text-foreground-muted"
          }`}
        >
          {degraded ? "Needs attention" : live ? "Configured" : "Not running"}
        </span>
      </div>
      <ul className="mt-4 divide-y divide-border rounded-lg border border-border">
        {rows.map((r) => (
          <CheckRow key={r.label} label={r.label} value={r.value} state={base} />
        ))}
      </ul>
      <p className="mt-3 text-[11px] text-foreground-muted">
        Configuration is not a connectivity test or proof of tool success. Review recorded Activity
        for actual outcomes and diagnostics for the current sandbox. Changes still require the
        applicable authorization and approval checks.
      </p>
    </section>
  );
}
