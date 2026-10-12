// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge Operator Console — Fleet Health (shift-triage home).
// Sandbox phase counts, the degraded list, and substrate scope. Operator
// truth: zeros are legitimate and useful here (operators count resources).

import Link from "next/link";
import { HonestState } from "@/components/honest-state";
import { Icon } from "@/components/icon";
import { listSandboxes, getInsights, listEgress, getOrchestrator, listSreActions } from "@/lib/bff";
import { headlampUrl } from "@/lib/config";
import { formatUptime } from "@/lib/format";

export const dynamic = "force-dynamic";

function phaseClass(phase: string | null): string {
  switch (phase) {
    case "Running":
    case "Ready":
      return "text-ok";
    case "Degraded":
    case "Failed":
      return "text-danger";
    case "Pending":
    case "Launching":
      return "text-warning";
    default:
      return "text-foreground-muted";
  }
}

export default async function FleetHealth() {
  const [sandboxRead, insightsRead, egressRead, orchestratorRead, sreRead] = await Promise.allSettled([
    listSandboxes(), getInsights(), listEgress(), getOrchestrator(), listSreActions(),
  ]);
  const error = sandboxRead.status === "rejected";
  const sandboxes = sandboxRead.status === "fulfilled" ? sandboxRead.value : [];
  const insights = insightsRead.status === "fulfilled" ? insightsRead.value : null;
  const egress = egressRead.status === "fulfilled" ? egressRead.value : null;
  const orchestrator = orchestratorRead.status === "fulfilled" ? orchestratorRead.value : null;
  const sreActions = sreRead.status === "fulfilled" ? sreRead.value : null;
  const sreSandbox = sandboxes.find((sandbox) => sandbox.name === "sre");
  const pendingSreActions = sreActions?.filter((action) => action.actionable).length ?? 0;

  const counts = sandboxes.reduce<Record<string, number>>((acc, s) => {
    const p = s.phase ?? "Unknown";
    acc[p] = (acc[p] ?? 0) + 1;
    return acc;
  }, {});
  const degraded = sandboxes.filter((s) => s.phase === "Degraded" || s.phase === "Failed");
  const egressPending = egress?.filter((e) => e.phase === "Pending").length ?? 0;
  const headlamp = headlampUrl();

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-xl font-semibold tracking-tight">Fleet Health</h1>
        <p className="mt-1 text-sm text-foreground-muted">
          Substrate triage — sandbox phases, degraded resources, and governance throughput.
        </p>
      </div>

      {error ? (
        <HonestState variant="not_wired" title="Cluster unreachable" detail="The Bridge backend can't reach the Kubernetes API right now." />
      ) : (
        <>
          <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-6">
            <Metric label="Sandboxes" value={sandboxes.length} mono />
            <Metric label="Running" value={(counts["Running"] ?? 0) + (counts["Ready"] ?? 0)} mono ok />
            <Metric label="Degraded" value={degraded.length} mono danger={degraded.length > 0} href="/console/fleet" />
            <Metric label="Egress grants" value={egress?.length ?? null} mono warn={egressPending > 0} href="/console/approvals" hint={egress === null ? "Egress inventory unavailable" : egressPending > 0 ? `${egressPending} pending` : undefined} />
            <Metric label="Amp. blocks" value={insights?.amplification_rejections ?? null} mono warn={(insights?.amplification_rejections ?? 0) > 0} hint={insights === null ? "Insights unavailable" : undefined} />
            <Metric label="Receipts issued" value={insights?.receipts_issued ?? null} mono href="/console/audit" hint={insights === null ? "Insights unavailable" : undefined} />
          </div>

          <section className="rounded-lg border border-border bg-surface">
            <div className="flex items-center justify-between border-b border-border px-5 py-3">
              <h2 className="text-sm font-semibold">Degraded / failed</h2>
              <Link href="/console/fleet" className="text-xs text-signal hover:underline">All sandboxes →</Link>
            </div>
            {degraded.length === 0 ? (
              <p className="px-5 py-6 text-sm text-foreground-muted">
                No sandboxes currently report Degraded or Failed. This does not verify model calls, tools, or completed work.
              </p>
            ) : (
              <ul className="divide-y divide-border">
                {degraded.map((s) => (
                  <li key={`${s.namespace}/${s.name}`} className="px-5 py-3">
                    <div className="flex items-center justify-between gap-3">
                      <span className="font-mono text-sm">{s.namespace}/{s.name}</span>
                      <span className={`text-xs font-medium ${phaseClass(s.phase)}`}>{s.phase}</span>
                    </div>
                    {s.message && <p className="mt-0.5 text-xs text-foreground-muted">{s.message}</p>}
                  </li>
                ))}
              </ul>
            )}
          </section>

          <section className="rounded-lg border border-border bg-surface px-5 py-4">
            <h2 className="text-sm font-semibold">Phase breakdown</h2>
            <div className="mt-3 flex flex-wrap gap-x-6 gap-y-2">
              {Object.entries(counts).length === 0 ? (
                <span className="text-sm text-foreground-muted">No sandboxes running — substrate idle.</span>
              ) : (
                Object.entries(counts).map(([phase, n]) => (
                  <span key={phase} className="inline-flex items-baseline gap-1.5 text-sm">
                    <span className={`font-semibold tabular-nums ${phaseClass(phase)}`}>{n}</span>
                    <span className="text-foreground-muted">{phase}</span>
                  </span>
                ))
              )}
            </div>
          </section>

          {/* Orchestrator health — the compose engine + which inference path it
              runs on, with the failover recommendation under load. */}
          {orchestrator ? (
            <section className={`rounded-lg border p-5 ${orchestrator.recommend_direct ? "border-amber-500/30 bg-amber-500/[0.04]" : "border-border bg-surface"}`}>
              <div className="flex flex-wrap items-start justify-between gap-3">
                <div>
                  <h2 className="flex items-center gap-2 text-sm font-semibold">
                    <Icon name="compass" size={14} /> Orchestrator (compose engine)
                  </h2>
                  <p className="mt-1 max-w-2xl text-xs text-foreground-muted">{orchestrator.note}</p>
                </div>
                <span className={`shrink-0 rounded-full px-2.5 py-0.5 text-[11px] font-semibold ${
                  orchestrator.mode !== "none" ? "bg-signal/15 text-signal"
                  : "bg-rose-500/15 text-rose-600"}`}>
                  {orchestrator.mode === "direct" ? "Configured direct" : orchestrator.mode === "sandbox" ? "Sandbox router" : "No path"}
                </span>
              </div>
              <dl className="mt-3 grid grid-cols-2 gap-3 sm:grid-cols-4 text-xs">
                <div><dt className="text-foreground-muted">Direct endpoint</dt><dd className="font-medium">{orchestrator.direct_configured ? "configured" : "not set"}</dd></div>
                <div><dt className="text-foreground-muted">Orchestrator sandbox</dt><dd className="font-medium">{orchestrator.sandbox_present ? (orchestrator.sandbox_phase ?? "present") : "absent"}</dd></div>
                <div><dt className="text-foreground-muted">Pod</dt><dd className="break-words font-medium">{orchestrator.sandbox_ready ?? "—"}{orchestrator.sandbox_waiting_reason ? ` · ${orchestrator.sandbox_waiting_reason}` : ""}</dd></div>
                <div title="Pod-status candidates, not verified model calls or a guarantee of independent capacity."><dt className="text-foreground-muted">Router candidates</dt><dd className="font-medium">{orchestrator.router_candidates}{orchestrator.router_candidates === 1 ? " path" : " paths"}</dd></div>
                <div><dt className="text-foreground-muted">Restarts</dt><dd className="font-medium">{orchestrator.sandbox_restarts ?? "—"}</dd></div>
                <div><dt className="text-foreground-muted">Node</dt><dd className="break-all font-mono text-[11px]">{orchestrator.sandbox_node ?? "—"}</dd></div>
                <div><dt className="text-foreground-muted">Uptime</dt><dd className="font-medium">{formatUptime(orchestrator.sandbox_uptime_seconds)}</dd></div>
                <div><dt className="text-foreground-muted">Evidence</dt><dd className="font-medium">Configuration &amp; pod status</dd></div>
              </dl>
            </section>
          ) : (
            <HonestState variant="not_wired" title="Orchestrator status unavailable" detail="The compose-engine status could not be retrieved. Its routing and readiness are unknown; refresh to retry." />
          )}

          {/* Cluster tools — deep-links to the K8s-native surfaces an operator
              reaches for, without leaving the console guessing what exists. */}
          <section className="rounded-lg border border-border bg-surface px-5 py-4">
            <h2 className="text-sm font-semibold">Cluster tools</h2>
            <div className="mt-3 grid gap-3 sm:grid-cols-3">
              <Link href="/console/troubleshooting" className="rounded-lg border border-border bg-surface p-3 transition hover:border-signal/40">
                <p className="flex items-center gap-1.5 text-sm font-medium"><Icon name="stethoscope" size={14} /> Diagnostics</p>
                <p className="mt-0.5 text-xs text-foreground-muted">Live &ldquo;what&rsquo;s failing right now&rdquo; scan — image pulls, crash loops, degraded sandboxes.</p>
              </Link>
              {headlamp ? (
                <a href={headlamp} target="_blank" rel="noreferrer" className="rounded-lg border border-border bg-surface p-3 transition hover:border-signal/40">
                  <p className="flex items-center gap-1.5 text-sm font-medium"><Icon name="eye" size={14} /> Headlamp ↗</p>
                  <p className="mt-0.5 text-xs text-foreground-muted">Open the Kubernetes dashboard for deep pod/node/event inspection.</p>
                </a>
              ) : (
                <div className="rounded-lg border border-dashed border-border bg-surface-muted/30 p-3">
                  <p className="flex items-center gap-1.5 text-sm font-medium text-foreground-muted"><Icon name="eye" size={14} /> Headlamp — not linked</p>
                  <p className="mt-0.5 text-xs text-foreground-muted" title="Operators: set the BRIDGE_HEADLAMP_URL environment variable to deep-link the Kubernetes dashboard here.">The Kubernetes dashboard isn&rsquo;t linked in this environment.</p>
                </div>
              )}
              <Link href="/console/sre-actions" className="rounded-lg border border-border bg-surface p-3 transition hover:border-signal/40">
                <p className="flex items-center gap-1.5 text-sm font-medium">
                  <Icon name="wrench" className="h-3.5 w-3.5" /> kars-SRE
                  {pendingSreActions > 0 && (
                    <span className="ml-auto rounded-full bg-warning/15 px-1.5 py-0.5 text-[10px] font-semibold text-warning">
                      {pendingSreActions} pending
                    </span>
                  )}
                </p>
                <p className="mt-0.5 text-xs text-foreground-muted">
                  {sreActions === null
                    ? "SRE proposal history unavailable. Open SRE Actions to retry."
                    : `${sreSandbox ? "" : "SRE agent not installed. "}${sreActions.length > 0
                      ? `${sreActions.length} recorded remediation proposal${sreActions.length === 1 ? "" : "s"} — review & decide.`
                      : sreSandbox
                        ? `No remediation proposals recorded. Sandbox phase: ${sreSandbox.phase ?? "Unknown"}.`
                        : "Install and enroll kars-SRE to enable diagnosis."}`}
                </p>
              </Link>
            </div>
          </section>
        </>
      )}
    </div>
  );
}

function Metric({
  label,
  value,
  mono,
  ok,
  danger,
  warn,
  href,
  hint,
}: {
  label: string;
  value: number | null;
  mono?: boolean;
  ok?: boolean;
  danger?: boolean;
  warn?: boolean;
  href?: string;
  hint?: string;
}) {
  const body = (
    <div className="h-full rounded-lg border border-border bg-surface p-4 transition hover:border-signal/40">
      <p
        className={[
          "text-2xl font-semibold",
          mono ? "font-mono" : "",
          danger ? "text-danger" : warn ? "text-warning" : ok ? "text-ok" : "text-foreground",
        ].join(" ")}
      >
        {value ?? "—"}
      </p>
      <p className="mt-0.5 text-xs text-foreground-muted">{label}</p>
      {hint && <p className="mt-0.5 text-[11px] font-medium text-warning">{hint}</p>}
    </div>
  );
  return href ? <Link href={href} className="block">{body}</Link> : body;
}
