"use client";

// kars Bridge — Mission blockers ("what the agent got stuck on"). Built from the
// REAL execution trace (per-tool events with their result previews), not from
// the agent's natural-language summary — which can confabulate ("please grant
// access") when a tool simply isn't available. Each failed tool call is
// classified honestly so the operator knows the actual cause and the actual
// remedy: a capability that isn't available on this cluster's provider, a real
// egress denial (with a one-click Grant), or a policy block.

import { useMemo } from "react";
import type { ActivityEvent, Approval } from "@/lib/types";
import { Icon, type IconName } from "@/components/icon";
import { ApprovalDecision } from "@/components/approval-decision";

type Kind = "capability" | "egress" | "policy" | "error";

type Blocker = {
  tool: string;
  kind: Kind;
  detail: string;
  host?: string; // for egress-class, the destination to grant
  count: number;
};

function classify(name: string, result: string): { kind: Kind; detail: string; host?: string } | null {
  const r = result.toLowerCase();
  // NB: the caller has already established this tool call FAILED (ok === false).
  // We only classify WHY, so we never gate on the presence of the word "error"
  // here — a real failure may not contain it, and a success that happens to
  // mention it never reaches this function.
  // Capability not available on this cluster's provider (e.g. Foundry-only
  // tools on a GitHub Models / Copilot cluster).
  if (r.includes("does not support") || r.includes("requires azure ai foundry") || r.includes("requires azure")) {
    const cap = name.replace(/^foundry[_.]?/i, "").replace(/_/g, " ");
    return {
      kind: "capability",
      detail: `“${cap || name}” isn't available on this cluster's model provider — it needs Azure AI Foundry. The agent has no real fallback, so it may improvise an explanation. Switch the cluster to Foundry, or compose missions without this capability.`,
    };
  }
  // Real egress denial by the kars boundary.
  if (r.includes("not on allowlist") || r.includes("egress policy") || r.includes("signed allowlist") || r.includes("kars egress")) {
    const host = extractHost(result);
    return { kind: "egress", detail: `The agent tried to reach ${host ?? "an external host"} and was denied by the network boundary.`, host };
  }
  // Tool/MCP policy block — the sandbox is purposefully limited. Widening a tool
  // policy is a BROAD-access change, so it's an operator action (not a one-click
  // user grant like egress): surface exactly what's needed and who can grant it.
  if (r.includes("blocked by policy")) {
    const m = result.match(/policy '([^']+)'/i);
    const tool = name.replace(/_/g, " ");
    return {
      kind: "policy",
      detail: `The agent tried to use “${tool}” but the tool policy${m ? ` “${m[1]}”` : ""} doesn't allow it — the sandbox is deliberately limited to what was granted. This is a broad-access change, so an operator needs to add this tool to the policy (or approve a policy that includes it). Alternatively, re-compose the mission without this tool.`,
    };
  }
  // Missing/unapproved skill surfaced in a tool result (rare at runtime — usually
  // caught at pre-flight). The trust gate refuses to mount an unapproved skill.
  if (r.includes("skill") && (r.includes("not found") || r.includes("not approved") || r.includes("not mounted"))) {
    return {
      kind: "policy",
      detail: `The agent needed a skill that isn't available to this sandbox. Skills must be uploaded and approved before the trust gate will mount them — ask an operator to approve the required skill, then re-run.`,
    };
  }
  // Generic upstream error (e.g. an HTTP 403 from the destination itself —
  // reached, but the server refused). Not a kars block.
  if (r.includes("403") || r.includes("forbidden")) {
    const host = extractHost(result);
    return { kind: "error", detail: `Reached ${host ?? "the destination"}, but it returned 403 Forbidden — this is the remote service refusing the request (often a missing header/credential), not a kars block.` };
  }
  if (r.includes("404") || r.includes("not found")) {
    const host = extractHost(result);
    return {
      kind: "error",
      detail: `Reached ${host ?? "the destination"}, but the requested path returned 404 Not Found. Network access worked; approving egress cannot fix this. The agent should use a valid URL or another authoritative source.`,
    };
  }
  return { kind: "error", detail: result.slice(0, 200) };
}

function extractHost(s: string): string | undefined {
  const url = s.match(/https?:\/\/([^/"\s]+)/i);
  if (url) return url[1];
  const host = s.match(/'([a-z0-9.-]+\.[a-z]{2,})'/i) || s.match(/([a-z0-9.-]+\.[a-z]{2,})(:\d+)?/i);
  return host?.[1];
}

const KIND_META: Record<Kind, { label: string; tone: string; glyph: IconName }> = {
  capability: { label: "Capability unavailable", tone: "border-amber-500/30 bg-amber-500/5 text-amber-600", glyph: "gear" },
  egress: { label: "Network denied", tone: "border-rose-500/30 bg-rose-500/5 text-rose-600", glyph: "globe" },
  policy: { label: "Policy block", tone: "border-sky-500/30 bg-sky-500/5 text-sky-600", glyph: "shield" },
  error: { label: "Remote error", tone: "border-border bg-surface-muted text-foreground-muted", glyph: "warning" },
};

export function MissionBlockers({
  approvals,
  activity,
  running,
  decider,
  authWired,
}: {
  ns: string;
  task: string;
  approvals: Approval[];
  activity: ActivityEvent[];
  running: boolean;
  decider: string;
  authWired: boolean;
}) {
  const blockers = useMemo<Blocker[]>(() => {
    const byKey = new Map<string, Blocker>();
    for (const e of activity ?? []) {
      const ev = e as unknown as { kind?: string; name?: string; result_preview?: string; ok?: boolean };
      if (ev.kind !== "tool" || !ev.name || !ev.result_preview) continue;
      // ONLY a tool call that actually FAILED is a blocker. The trace carries a
      // real success flag (ok) per call; gate on it instead of string-matching
      // "error" in the result — a successful call whose output merely contains
      // the word "error" (e.g. a status line "ERROR: none") is NOT a failure and
      // must never surface as "what the agent got stuck on" on a delivered run.
      if (ev.ok !== false) continue;
      const c = classify(ev.name, ev.result_preview);
      if (!c) continue;
      const key = `${ev.name}|${c.kind}|${c.host ?? ""}`;
      const existing = byKey.get(key);
      if (existing) existing.count += 1;
      else byKey.set(key, { tool: ev.name, kind: c.kind, detail: c.detail, host: c.host, count: 1 });
    }
    // Order: egress (actionable) → capability → policy → error.
    const order: Record<Kind, number> = { egress: 0, capability: 1, policy: 2, error: 3 };
    return [...byKey.values()].sort((a, b) => order[a.kind] - order[b.kind]);
  }, [activity]);

  // The single source of truth for an egress denial is the KarsApproval the
  // controller opens for it — the SAME object the Approvals panel (below) and
  // the fleet inbox act on. We reflect its live state here instead of offering
  // a second, competing "grant" button that races the approval.
  function egressApprovalFor(host?: string): Approval | undefined {
    if (!host) return undefined;
    const h = host.toLowerCase();
    return approvals
      .filter(
        (a) =>
        a.action_kind === "egress" &&
        ((a.summary ?? "").toLowerCase().includes(h) || (a.detail ?? "").toLowerCase().includes(h)),
      )
      .sort((a, b) => {
        const rank = (approval: Approval) =>
          approval.actionable ? 3 : approval.phase === "Approved" ? 2 : approval.phase === "Denied" ? 1 : 0;
        return rank(b) - rank(a) || Number(b.resource_version) - Number(a.resource_version);
      })[0];
  }
  const hasEgressBlock = blockers.some((blocker) => blocker.kind === "egress");
  const hasOnlyRemoteErrors = blockers.every((blocker) => blocker.kind === "error");

  if (blockers.length === 0) return null;

  return (
    <section className="rounded-xl border border-warning/30 bg-warning/[0.04] p-6">
      <div className="flex items-start gap-2">
        <Icon name="warning" size={16} />
        <div>
          <h2 className="text-sm font-semibold">What the agent got stuck on</h2>
          <p className="mt-0.5 max-w-xl text-xs text-foreground-muted">
            Read from the real execution trace — not the agent&rsquo;s own summary, which can
            misattribute a missing capability as an &ldquo;access request.&rdquo; Here&rsquo;s the
            actual cause and what you can do.
          </p>
        </div>
      </div>
      <ul className="mt-4 space-y-2.5">
        {blockers.map((b) => {
          const m = KIND_META[b.kind];
          const appr = b.kind === "egress" ? egressApprovalFor(b.host) : undefined;
          return (
            <li key={`${b.tool}-${b.kind}-${b.host ?? ""}`} className={`rounded-lg border p-3 ${m.tone.replace(/text-[a-z0-9/-]+/, "")}`}>
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0">
                  <p className="flex items-center gap-2 text-sm font-medium">
                    <Icon name={m.glyph} size={13} />
                    <span className="font-mono text-xs">{b.tool}</span>
                    <span className={`rounded-full border px-1.5 py-0.5 text-[10px] font-medium ${m.tone}`}>{m.label}</span>
                    {b.count > 1 && <span className="text-[10px] text-foreground-muted">×{b.count}</span>}
                  </p>
                  <p className="mt-1 text-xs text-foreground-muted">{b.detail}</p>
                </div>
                {b.kind === "egress" && b.host && (
                  <EgressState appr={appr} running={running} decider={decider} authWired={authWired} />
                )}
              </div>
            </li>
          );
        })}
      </ul>
      {hasEgressBlock ? (
        <p className="mt-3 text-[11px] text-foreground-muted">
          Only items marked <strong className="text-foreground">Network denied</strong> require an approval.
          Decide those in Approvals below (or your inbox); remote HTTP errors need a corrected URL or source instead.
        </p>
      ) : hasOnlyRemoteErrors ? (
        <p className="mt-3 text-[11px] text-foreground-muted">
          No approval is required for these calls: the network path worked and the remote service returned an error.
          Re-run with a corrected URL or another authoritative source.
        </p>
      ) : (
        <p className="mt-3 text-[11px] text-foreground-muted">
          Resolve the capability or policy item described above; remote HTTP errors cannot be fixed by approving egress.
        </p>
      )}
    </section>
  );
}

/** Reflects the live state of the egress KarsApproval — never a second grant
 * button. The one action lives in the Approvals panel / inbox. */
function EgressState({
  appr,
  running,
  decider,
  authWired,
}: {
  appr?: Approval;
  running: boolean;
  decider: string;
  authWired: boolean;
}) {
  if (!appr) {
    return (
      <span className="shrink-0 rounded-lg border border-border bg-surface-muted px-3 py-1.5 text-xs font-medium text-foreground-muted">
        {running ? "Surfacing to your inbox…" : "No active approval — rerun if still needed"}
      </span>
    );
  }
  if (appr.phase === "Approved") {
    return (
      <span className="shrink-0 rounded-lg border border-ok/40 bg-ok/10 px-3 py-1.5 text-xs font-medium text-ok">
        ✓ Approved — the agent will retry
      </span>
    );
  }
  if (appr.phase === "Denied") {
    return (
      <span className="shrink-0 rounded-lg border border-rose-500/40 bg-rose-500/10 px-3 py-1.5 text-xs font-medium text-rose-600">
        Denied
      </span>
    );
  }
  if (appr.phase === "Expired" || appr.phase === "Stale") {
    return (
      <span className="shrink-0 rounded-lg border border-border bg-surface-muted px-3 py-1.5 text-xs font-medium text-foreground-muted">
        {appr.decider ? `Previously ${appr.phase.toLowerCase()} after decision` : appr.phase}
      </span>
    );
  }
  if (appr.actionable) {
    return (
      <div className="shrink-0">
        <ApprovalDecision
          name={appr.name}
          decider={decider}
          authWired={authWired}
          resourceVersion={appr.resource_version}
          boundEnvelopeDigest={appr.bound_envelope_digest}
          compact
        />
      </div>
    );
  }
  return (
    <span className="shrink-0 rounded-lg border border-signal/40 bg-signal/10 px-3 py-1.5 text-xs font-medium text-signal">
      Awaiting your approval ↓
    </span>
  );
}
