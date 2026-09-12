// kars Bridge Workspace — Pre-flight access readiness (design note FL5).
//
// Before (and during) a run, the operator must be able to see — at a glance —
// that the agent has every access it needs, and nothing it doesn't. This panel
// turns the composed envelope into an explicit checklist: model path, tool
// policy, connected services, network egress, isolation, shared memory. Honest
// states: pre-launch each line reads "verified at launch" (it's a plan);
// once the controller has materialised the sandbox the same lines read
// "verified" (the boundary is live). A degraded sandbox surfaces as attention.

import type { Composition } from "@/lib/types";
import { egressScope, humanizeMcp } from "@/lib/format";

type Readiness = "verified" | "pending" | "attention";

function StatusDot({ state }: { state: Readiness }) {
  const map: Record<Readiness, { cls: string; glyph: string; label: string }> = {
    verified: { cls: "text-emerald-600", glyph: "✓", label: "Verified" },
    pending: { cls: "text-foreground-muted", glyph: "○", label: "At launch" },
    attention: { cls: "text-amber-600", glyph: "!", label: "Attention" },
  };
  const m = map[state];
  return (
    <span className={`inline-flex items-center gap-1.5 text-xs font-medium ${m.cls}`}>
      <span
        aria-hidden
        className={`grid h-4 w-4 place-items-center rounded-full text-[10px] ${state === "verified" ? "bg-emerald-500/10" : state === "attention" ? "bg-amber-500/10" : "bg-surface-muted"}`}
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
  // A delivered mission ran to completion behind the enforced boundary, so its
  // accesses were verified even though the sandbox has since been torn down and
  // the execution phase has returned to idle.
  const live =
    delivered ||
    (launched && (executionPhase === "Running" || executionPhase === "Ready" || executionPhase === "Succeeded"));
  const base: Readiness = degraded ? "attention" : live ? "verified" : "pending";

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
      : "model path only — all else denied",
  });
  if (composition.memory) rows.push({ label: "Shared memory", value: composition.memory });

  const allVerified = base === "verified";
  const headline = degraded
    ? "One or more accesses need attention"
    : failed
      ? "The access boundary was verified; execution failed afterward"
    : live
      ? "Every access the agent needs is verified and bounded"
      : "Access plan — verified the moment this mission launches";

  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">Pre-flight access</h2>
          <p className="mt-0.5 text-xs text-foreground-muted">{headline}</p>
        </div>
        <span
          className={`shrink-0 rounded-full border px-2.5 py-1 text-xs font-medium ${
            allVerified
              ? "border-emerald-500/30 bg-emerald-500/10 text-emerald-600"
              : degraded
                ? "border-amber-500/30 bg-amber-500/10 text-amber-600"
                : "border-border bg-surface-muted text-foreground-muted"
          }`}
        >
          {allVerified ? "Ready" : degraded ? "Needs attention" : "Pending launch"}
        </span>
      </div>
      <ul className="mt-4 divide-y divide-border rounded-lg border border-border">
        {rows.map((r) => (
          <CheckRow key={r.label} label={r.label} value={r.value} state={base} />
        ))}
      </ul>
      <p className="mt-3 text-[11px] text-foreground-muted">
        Each line is enforced at the sandbox boundary — the agent cannot reach anything not listed
        here. Granting more happens through an approval you&apos;ll see in your Inbox.
      </p>
    </section>
  );
}
