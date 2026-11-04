// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { PageHeader, Stat, Badge } from "@/components/ui";
import { DatapathSetup } from "@/components/datapath-setup";
import { LiveRefresh } from "@/components/live-refresh";
import { getDatapathWitness } from "@/lib/bff";
import { witnessPresentation } from "@/lib/datapath-witness";
import { canAdminister } from "@/lib/session";
import type { DatapathWitness, DatapathWitnessSandbox } from "@/lib/types";

export const dynamic = "force-dynamic";

function verdictTone(v: string): "ok" | "warn" | "muted" {
  if (v === "BEYOND-DECLARED" || v === "UNAVAILABLE") return "warn";
  return "muted";
}

function verdictLabel(v: string): string {
  if (v === "COMPLIANT" || v === "NO-BEYOND-OBSERVED") return "No beyond-declared DNS in sample";
  if (v === "BEYOND-DECLARED") return "DNS beyond baseline";
  if (v === "NO-TRAFFIC") return "No external events / unproven";
  if (v === "LEARN") return "Learn mode";
  if (v === "UNAVAILABLE") return "Baseline unavailable / comparison unknown";
  return v;
}

export default async function DatapathWitnessPage() {
  const isAdmin = await canAdminister();
  let witness: DatapathWitness | null = null;
  let error = false;
  try {
    witness = await getDatapathWitness();
  } catch {
    error = true;
  }

  const presentation = witnessPresentation(witness);
  const sandboxes = presentation.fresh ? witness?.sandboxes ?? [] : [];
  const beyond = sandboxes.filter((s) => s.verdict === "BEYOND-DECLARED").length;
  const learn = sandboxes.filter((s) => s.verdict === "LEARN").length;
  const unavailable = sandboxes.filter((s) => s.verdict === "UNAVAILABLE").length;

  return (
    <div className="space-y-6">
      <LiveRefresh active intervalMs={5000} />
      <PageHeader
        eyebrow="Operator Console"
        title="Datapath witness"
        lead="Optional, sampled kernel DNS/TCP observations compared with controller-declared baselines. Observational, not enforcing, and not a complete-kernel-coverage attestation. Core Kars does not require Bridge or this add-on."
      />

      {error && (
        <div className="rounded-xl border border-danger/30 bg-danger/[0.06] px-4 py-3 text-sm text-foreground-muted">
          Couldn&apos;t reach the cluster to read the witness. Check the BFF&apos;s cluster wiring.
        </div>
      )}

      {!error && (
        <div className="kb-card space-y-3 p-5">
          <Badge tone={presentation.state === "invalid" || presentation.state === "unavailable" || presentation.state === "stale" ? "warn" : "muted"}>
            {presentation.label}
          </Badge>
          <p className="text-sm text-foreground-muted">{presentation.diagnostic}</p>
          <p className="text-xs text-foreground-muted">
            As of latest refresh: operator intent {presentation.requested} · Installation: unknown (Bridge reads reports,
            not Helm or workload inventory). Missing reports can coexist with running legacy observers.
          </p>
          {witness?.generated_at && (
            <p className="text-xs text-foreground-muted">
              Report timestamp: {witness.generated_at} · freshness limit: 180 seconds.
            </p>
          )}
        </div>
      )}
      <div className="kb-card space-y-2 p-4 text-sm text-foreground-muted">
        <p className="font-medium text-foreground">Looking for a Team or Mission agent?</p>
        <p>
          Temporary workers can appear only while their sandboxes exist during a capture window
          and are in the configured scope. Finished workers are not a permanent inventory here.
          An unavailable sample does not mean that no agents are running.
        </p>
        <p>
          <a href="/workspace/agents" className="underline">Active agents</a> shows assignment-linked work.
          {" "}<a href="/console/fleet" className="underline">Sandboxes</a> shows current infrastructure records.
          {" "}<a href="/workspace/teams" className="underline">Teams</a> and
          {" "}<a href="/workspace/missions" className="underline">Missions</a> retain run history and deliverables.
        </p>
      </div>

      {isAdmin ? <DatapathSetup ownerRelease={witness?.owner_release} /> : (
        <p className="text-sm text-foreground-muted">
          Ask a Bridge admin for setup details. Enablement and removal must be performed by a cluster operator outside Bridge.
        </p>
      )}

      {!error && presentation.fresh && (
        <>
          <div className="kb-card space-y-2 p-4 text-sm text-foreground-muted">
            {witness?.scope === "all" ? (
              <p>
                <span className="font-medium text-foreground">All-sandbox scope / dynamic sampling.</span>{" "}
                The collector rediscovers KarsSandbox objects in kars-system each sample,
                including new Team and Mission workers, and bounds capture to their namespaces.
                These rows are the reported sample, not a live fleet inventory or proof that
                every sandbox, node, or event was observed.
              </p>
            ) : (
              <p>
                <span className="font-medium text-foreground">Selected sandboxes only.</span>{" "}
                This is not a fleet-wide feed. Only the listed sandboxes are in scope; activity in
                other sandboxes, including Team and Mission workers, is not included unless selected.
              </p>
            )}
            <p>
              In-scope events include internal traffic, DNS replies, and TCP accept/close events.
              The rows show only non-cluster DNS queries and TCP connect attempts to global IP
              addresses. A nonzero event count can therefore coexist with empty external-traffic rows.
            </p>
          </div>

          <div className="grid grid-cols-2 gap-3 sm:grid-cols-5">
            <Stat label="Sandboxes in sample" value={sandboxes.length} />
            <Stat label="DNS beyond baseline" value={beyond} accent={beyond > 0} />
            <Stat label="In-scope events" value={witness?.event_count ?? 0} />
            <Stat label="Learn mode" value={learn} />
            <Stat label="Baseline unavailable" value={unavailable} accent={unavailable > 0} />
          </div>

          <div className="flex flex-wrap items-center gap-2 text-xs text-foreground-muted">
            Partial sample, not a live stream
            {witness?.window_seconds && <span>· {witness.window_seconds}s capture window</span>}
            <span>· {witness?.nodes_with_events?.length ?? 0} nodes with in-scope events /
              {" "}{witness?.nodes_targeted?.length ?? 0} targeted</span>
          </div>

          {sandboxes.length === 0 ? (
            <div className="rounded-xl border border-dashed border-border bg-surface-muted/30 px-4 py-8 text-center text-sm text-foreground-muted">
              No sandbox records in this sample. This does not demonstrate observation coverage.
            </div>
          ) : (
            <ul className="space-y-3">
              {sandboxes.map((s) => (
                <WitnessRow key={s.sandbox_uid ?? s.namespace} s={s} />
              ))}
            </ul>
          )}

          <p className="text-xs leading-relaxed text-foreground-muted">
            <span className="font-medium text-foreground">How to read this.</span> DNS = host
            intent, not proof of a connection. External TCP connect events include failed attempts
            and are not correlated to DNS names. The comparison uses the controller&apos;s compiled
            baseline, not its runtime approval overlays, and does not verify a signature. Learn is
            an explicit enforcement mode; an empty Strict baseline is deny-all, not unconstrained.
            An unavailable or invalid baseline is neither Learn nor Strict deny-all: observations
            remain visible, but no beyond-baseline comparison can be made for that row.
            Neither empty traffic, ready pods, nor absence of beyond-baseline DNS proves complete kernel coverage.
            UDP other than DNS, cached DNS, attribution gaps, and gaps between windows remain outside
            this evidence. This page refreshes automatically while visible; capture remains sampled.
          </p>
        </>
      )}
    </div>
  );
}

function WitnessRow({ s }: { s: DatapathWitnessSandbox }) {
  const unavailable = s.verdict === "UNAVAILABLE";
  return (
    <li className="kb-card p-4">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="font-mono text-sm font-medium">
            <a href={`/console/fleet?q=${encodeURIComponent(s.sandbox)}`} className="underline">
              {s.sandbox}
            </a>
          </p>
          <p className="mt-0.5 font-mono text-[11px] text-foreground-muted">{s.namespace}</p>
          {s.sandbox_uid && <p className="mt-0.5 break-all font-mono text-[11px] text-foreground-muted">CR UID: {s.sandbox_uid}</p>}
        </div>
        <Badge tone={verdictTone(s.verdict)} dot>
          {verdictLabel(s.verdict)}
        </Badge>
      </div>

      {unavailable && (
        <p className="mt-3 text-xs text-warning">
          {s.declaration_diagnostic === "declaration_invalid"
            ? "Controller declaration is invalid (declaration_invalid)."
            : "Controller baseline could not be read (baseline_unavailable)."}{" "}
          Observations are retained; comparison is unavailable, not evidence of allowed or denied traffic.
        </p>
      )}
      <div className="mt-3 grid gap-3 sm:grid-cols-3">
        <HostSet label="Declared baseline" hosts={s.declared_hosts}
          empty={unavailable ? "unavailable / not compared" : s.egress_mode === "Strict" ? "empty baseline (Strict deny-all)" : "empty baseline (Learn)"} />
        <HostSet
          label="Observed external DNS"
          hosts={s.observed_dns}
          empty="none in window"
          highlight={new Set(s.beyond_declared)}
        />
        <div className="min-w-0">
          <p className="mb-1 text-[11px] font-semibold uppercase tracking-wide text-foreground-muted">
            External TCP connect events (attempts)
          </p>
          <p className="text-sm tabular-nums">{s.observed_connects}</p>
          {s.beyond_declared.length > 0 && (
            <p className="mt-1 text-[11px] text-warning">
              {s.beyond_declared.length} beyond declared
            </p>
          )}
        </div>
      </div>
    </li>
  );
}

/** The K8s stub resolver expands a ClusterIP name with the node's DNS search
 *  domain, so the eBPF witness observes both the real name and search-expanded
 *  variants like `foo.ns.svc.cluster.local.<node-suffix>.internal.cloudapp.net`.
 *  Trim back to the meaningful in-cluster name so the list is readable (the full
 *  observed string stays available on hover). */
function cleanHost(h: string): string {
  const marker = ".svc.cluster.local";
  const idx = h.indexOf(marker);
  if (idx !== -1) return h.slice(0, idx + marker.length);
  return h;
}

function HostSet({
  label,
  hosts,
  empty,
  highlight,
}: {
  label: string;
  hosts: string[];
  empty: string;
  highlight?: Set<string>;
}) {
  // Collapse search-domain-expanded duplicates to one readable entry, carrying
  // the beyond-declared flag if ANY raw variant was flagged, and keeping the
  // longest raw form for the hover title.
  const grouped = new Map<string, { flagged: boolean; raw: string }>();
  for (const h of hosts) {
    const c = cleanHost(h);
    const prev = grouped.get(c);
    grouped.set(c, {
      flagged: (prev?.flagged ?? false) || (highlight?.has(h) ?? false),
      raw: prev && prev.raw.length >= h.length ? prev.raw : h,
    });
  }
  const items = Array.from(grouped.entries());

  return (
    <div className="min-w-0">
      <p className="mb-1 text-[11px] font-semibold uppercase tracking-wide text-foreground-muted">
        {label}
      </p>
      {items.length === 0 ? (
        <p className="text-xs italic text-foreground-muted/70">{empty}</p>
      ) : (
        <ul className="flex flex-col gap-1">
          {items.map(([display, { flagged, raw }]) => (
            <li
              key={display}
              title={raw}
              className={`block break-all rounded border px-1.5 py-0.5 font-mono text-[11px] leading-snug ${
                flagged
                  ? "border-warning/40 bg-warning/10 text-warning"
                  : "border-border bg-surface-muted text-foreground-muted"
              }`}
            >
              {display}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
