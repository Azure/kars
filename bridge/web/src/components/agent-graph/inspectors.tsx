// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type * as React from "react";
import { Icon } from "@/components/icon";
import type { AgentIdentity, Receipt } from "@/lib/types";
import { actionRound, exactActionQuery, formatLastActivity, phaseKind, phaseTone } from "./activity";
import { buildExecutionOverview } from "./execution";
import type { AgentExecution, GraphSelection } from "./types";

export function SelectionInspector({ selection }: { selection: GraphSelection }) {
  if (selection.kind === "agent") return <AgentInspector agent={selection.agent} />;
  if (selection.kind === "edge") {
    const childKind = phaseKind(selection.child.phase);
    return (
      <InspectorShell eyebrow="Delegation relationship" title={`${selection.parent.displayName} → ${selection.child.displayName}`}>
        <p className="text-xs leading-relaxed text-foreground-muted">
          Runtime metadata identifies{" "}
          <span className="font-medium text-foreground">{selection.parent.displayName}</span>
          {" as the parent of "}
          <span className="font-medium text-foreground">{selection.child.displayName}</span>
          {selection.child.role ? ` as ${selection.child.role}` : ""}. The child is currently{" "}
          <span className={childKind === "failed" ? "font-medium text-danger" : "font-medium text-foreground"}>
            {selection.child.phase}
          </span>.
        </p>
        <p className="mt-2 text-[10px] leading-relaxed text-foreground-muted">
          No delegation-event ledger is attached to this trace. The drill-down below focuses the child agent&apos;s exact retained activity, not an inferred edge event.
        </p>
        <div className="mt-3 grid gap-2 sm:grid-cols-3">
          <Metric label="Child runtime" value={selection.child.runtime ?? "Not reported"} small />
          <Metric label="Child model" value={selection.child.model ?? "Not reported"} small />
          <Metric label="Latest activity" value={formatLastActivity(selection.child.lastActivity)} small />
        </div>
      </InspectorShell>
    );
  }
  if (selection.kind === "specialist-aggregate") {
    return (
      <InspectorShell eyebrow="Folded specialist group" title={selection.label}>
        <p className="text-xs text-foreground-muted">
          Specialists sharing <span className="font-medium text-foreground">{selection.parent.displayName}</span> as their runtime parent.
        </p>
        <FoldedList
          items={selection.agents.map((agent) => ({
            title: `${agent.displayName} · ${agent.role}`,
            detail: `${agent.technicalName} · ${agent.runtime ?? "runtime not reported"} · ${agent.model ?? "model not reported"} · ${agent.phase}`,
            failed: phaseKind(agent.phase) === "failed",
          }))}
        />
      </InspectorShell>
    );
  }
  if (selection.kind === "action") {
    const action = selection.action;
    return (
      <InspectorShell eyebrow={action.source === "legacy" ? "Recorded action" : action.source} title={action.human}>
        <div className="flex flex-wrap gap-2 text-[10px] text-foreground-muted">
          <span className="rounded-full border border-border px-2 py-1">Agent {selection.agent.displayName}</span>
          <span className="rounded-full border border-border px-2 py-1">{action.source === "legacy" ? "Round" : "Router round"} {actionRound(action)}</span>
          <span className="rounded-full border border-border px-2 py-1">{action.ms === null ? "Duration unavailable" : `${action.ms} ms`}</span>
          <span className={`rounded-full border px-2 py-1 ${
            action.ok === false ? "border-danger/30 text-danger" : action.ok === true ? "border-signal/30 text-signal" : "border-border"
          }`}>
            {action.source === "model-proposed" ? "Proposal only" : action.source === "router-upstream" ? "Router response"
              : action.source === "router-policy" ? "Router policy observation" : action.source === "router" ? "Router fetch observation"
              : action.source === "legacy" ? action.ok === false ? "Failed" : action.ok === true ? "Succeeded" : "Model round"
                : action.ok === false ? "Failure reported" : action.ok === true ? "Success reported" : "Outcome unavailable"}
          </span>
          <span className="rounded-full border border-border px-2 py-1">{action.ts ? formatLastActivity(action.ts) : "Timestamp unavailable"}</span>
        </div>
        <p className="mt-3 break-all rounded-lg bg-surface-muted/45 px-3 py-2 font-mono text-[10px]">
          <span className="font-sans font-medium text-foreground-muted">Raw tool: </span>{action.raw}
        </p>
        <div className="mt-2 grid gap-2 text-[10px] sm:grid-cols-2">
          <DetailBlock label="Arguments" value={action.args || "No input preview retained"} />
          <DetailBlock label={action.ok === false ? "Failure / result" : "Result"} value={action.result || "No result preview retained"} />
        </div>
      </InspectorShell>
    );
  }
  if (selection.kind === "folded-actions") {
    return (
      <InspectorShell eyebrow="Folded action cluster" title={`${selection.actions.length} earlier actions`}>
        <FoldedList
          items={[...selection.actions].reverse().map((action) => ({
            title: action.human,
            detail: `${action.raw} · round ${actionRound(action)} · ${action.ms === null ? "duration unavailable" : `${action.ms} ms`} · ${action.ts ? formatLastActivity(action.ts) : "timestamp unavailable"}`,
            failed: action.ok === false,
          }))}
        />
      </InspectorShell>
    );
  }
  if (selection.kind === "destination") {
    return (
      <InspectorShell eyebrow="Network destination" title={selection.destination}>
        <p className="text-xs text-foreground-muted">
          Referenced by retained action evidence from <span className="font-medium text-foreground">{selection.agent.displayName}</span>.
        </p>
      </InspectorShell>
    );
  }
  return (
    <InspectorShell eyebrow="Folded destination cluster" title={`${selection.destinations.length} additional destinations`}>
      <FoldedList items={selection.destinations.map((destination) => ({ title: destination, detail: "Referenced in retained action evidence" }))} />
    </InspectorShell>
  );
}

export function ExecutionOverview({ agents, onInspect }: { agents: AgentExecution[]; onInspect?: (query: string) => void }) {
  const overview = buildExecutionOverview(agents);
  return (
    <InspectorShell eyebrow="All producers · retained and live records" title="Execution overview">
      <p className="text-xs text-foreground-muted">
        Records from {overview.recorded.length} of {agents.length} agents or phases. Worker activity is not attributed to the principal.
        {" "}These are recorded counts, not complete execution or usage totals.
      </p>
      {overview.unavailable > 0 && <p className="mt-1 text-xs text-foreground-muted">
        Detailed trace unavailable for {overview.unavailable} agent{overview.unavailable === 1 ? "" : "s"} or phases; missing records do not mean zero execution.
      </p>}
      <div className="mt-3 grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
        <Metric label="Retained rounds" value={overview.rounds ?? "Not recorded"} />
        <Metric label="Reported tool results" value={overview.toolCalls ?? "Not recorded"} />
        <Metric label="Reported failures" value={overview.failures ?? "Not recorded"} danger={(overview.failures ?? 0) > 0} />
        <Metric label="Latest timestamped activity" value={overview.latest ? formatLastActivity(overview.latest.action.ts) : overview.recorded.length ? "Timestamp unavailable" : "No recorded activity"} small />
      </div>
      <div className="mt-2 rounded-lg border border-border bg-surface px-3 py-2">
        <p className="text-[9px] font-medium uppercase tracking-wide text-foreground-muted">Latest timestamped action</p>
        {overview.latest ? <button type="button" className="mt-0.5 text-left text-xs font-medium text-signal hover:underline"
          onClick={() => onInspect?.(exactActionQuery(overview.latest!.agent, overview.latest!.action))}>
          {overview.latest.agent.displayName}: {overview.latest.action.human} · Inspect record
        </button> : <p className="mt-0.5 text-xs">{overview.recorded.length ? "Timestamp unavailable; inspect each producer below" : "Detailed trace unavailable"}</p>}
        {overview.untimestamped && <p className="mt-1 text-xs text-foreground-muted">Some records have no timestamp; no newest action across producers can be established for those records.</p>}
      </div>
      <ul className="mt-3 space-y-2">
        {overview.recorded.map(agent => {
          const latest = agent.actions.at(-1)!;
          return <li key={agent.id} className="rounded-lg border border-border bg-surface px-3 py-2">
            <button type="button" className="text-left text-xs font-medium text-signal hover:underline" onClick={() => onInspect?.(agent.traceQuery)}>
              {agent.displayName} · {agent.actions.length} records · Inspect producer
            </button>
            <p className="mt-1 text-[10px] text-foreground-muted">Latest producer record: {latest.human} · {latest.ts ? formatLastActivity(latest.ts) : "Timestamp unavailable"}</p>
          </li>;
        })}
      </ul>
    </InspectorShell>
  );
}

function AgentInspector({ agent }: { agent: AgentExecution }) {
  const latest = agent.actions.at(-1);
  return (
    <InspectorShell eyebrow={agent.role === "Recorded producer" ? "Recorded producer" : agent.isPrincipal ? "Principal agent" : "Specialist agent"} title={agent.displayName}>
      <div className="flex flex-wrap items-center gap-2">
        <span className={`rounded-full border px-2 py-1 text-[10px] font-medium ${phaseTone(agent.phase)}`}>
          {agent.phase}
        </span>
        <span className="text-[11px] text-foreground-muted">{agent.relationship}</span>
      </div>
      <dl className="mt-3 grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
        <Metric label="Role" value={agent.role} small />
        <Metric label="Runtime" value={agent.runtime ?? "Not reported"} small />
        <Metric label="Model" value={agent.model ?? "Not reported"} small />
        <Metric label="Exact agent name" value={agent.technicalName} small />
      </dl>
      <div className="mt-2 grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
        <Metric label="Retained rounds" value={agent.actions.length ? agent.rounds : "Not recorded"} />
        <Metric label={agent.traceQuery.startsWith("observations:") ? "Reported results" : "Tool calls"} value={agent.actions.length ? agent.toolCalls : "Not recorded"} />
        <Metric label="Failures" value={agent.actions.length ? agent.failures : "Not recorded"} danger={agent.failures > 0} />
        <Metric label="Latest activity" value={latest && !agent.lastActivity ? "Timestamp unavailable" : formatLastActivity(agent.lastActivity)} small />
      </div>
      <div className="mt-2 rounded-lg border border-border bg-surface px-3 py-2">
        <p className="text-[9px] font-medium uppercase tracking-wide text-foreground-muted">Latest action</p>
        <p className="mt-0.5 text-xs font-medium">{latest?.human ?? "Detailed trace unavailable"}</p>
        {!latest && <p className="mt-1 text-xs text-foreground-muted">
          Missing action records do not mean zero execution. Any recorded run totals are separate from this detailed trace.
        </p>}
      </div>
    </InspectorShell>
  );
}

function InspectorShell({
  eyebrow,
  title,
  children,
}: {
  eyebrow: string;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <div className="mt-4 rounded-xl border border-signal/25 bg-signal/[0.035] p-4" aria-live="polite">
      <p className="text-[9px] font-semibold uppercase tracking-[0.16em] text-signal">{eyebrow}</p>
      <h3 className="mt-0.5 break-words text-sm font-semibold">{title}</h3>
      <div className="mt-2">{children}</div>
    </div>
  );
}

function DetailBlock({ label, value }: { label: string; value: string }) {
  return (
    <p className="break-words rounded-lg bg-surface-muted/45 px-3 py-2">
      <span className="font-medium text-foreground-muted">{label}: </span>
      <span className="font-mono">{value}</span>
    </p>
  );
}

function FoldedList({
  items,
}: {
  items: Array<{ title: string; detail: string; failed?: boolean }>;
}) {
  return (
    <ol className="max-h-56 space-y-1.5 overflow-y-auto pr-1">
      {items.map((item, index) => (
        <li key={`${item.title}-${index}`} className="flex gap-2 rounded-lg border border-border bg-surface px-3 py-2">
          <span className={`mt-1 h-1.5 w-1.5 shrink-0 rounded-full ${item.failed ? "bg-danger" : "bg-signal"}`} />
          <span className="min-w-0">
            <span className="block text-[11px] font-medium">{item.title}</span>
            <span className="block break-all font-mono text-[9px] text-foreground-muted">{item.detail}</span>
          </span>
        </li>
      ))}
    </ol>
  );
}

function Metric({
  label,
  value,
  danger = false,
  small = false,
}: {
  label: string;
  value: string | number;
  danger?: boolean;
  small?: boolean;
}) {
  return (
    <div className="rounded-md border border-border bg-surface px-2 py-1.5">
      <dt className="text-[9px] uppercase tracking-wide text-foreground-muted">{label}</dt>
      <dd className={`mt-0.5 break-words ${small ? "text-[10px] leading-tight" : "font-semibold tabular-nums"} ${danger ? "text-danger" : ""}`}>
        {value}
      </dd>
    </div>
  );
}

export function EvidenceReferences({
  envelopeDigest,
  identity,
  nonPrincipalCount,
  receipt,
}: {
  envelopeDigest: string | null;
  identity: AgentIdentity | null;
  nonPrincipalCount: number;
  receipt: Receipt | null;
}) {
  const short = (value: string, head = 10, tail = 6) =>
    value.length > head + tail + 1
      ? `${value.slice(0, head)}...${value.slice(-tail)}`
      : value;
  const points = [
    {
      when: "Run record",
      title: "Trust envelope digest",
      detail: envelopeDigest
        ? `Recorded digest ${short(envelopeDigest.replace(/^sha256:/, ""))}. A digest alone does not verify a signature or the checks performed before launch.`
        : "No trust envelope digest is available for this run.",
      available: Boolean(envelopeDigest),
    },
    {
      when: "Identity record",
      title: "Agent mesh identity (DID)",
      detail: identity?.did
        ? `Reported identity ${short(identity.did, 16, 8)}${identity.reputation_score != null ? `, reputation ${identity.reputation_score}` : ""}. This reference is not a verified registration proof.`
        : "No mesh identity reference is available in this view.",
      available: Boolean(identity?.did),
    },
    {
      when: "Execution graph",
      title: "Specialist and producer references",
      detail:
        nonPrincipalCount > 0
          ? `${nonPrincipalCount} non-principal graph ${nonPrincipalCount === 1 ? "entry" : "entries"}. These can include planned phases and observed producers; the count does not establish sandbox creation or authority checks.`
          : "No non-principal graph entries are available. This does not establish whether delegation occurred.",
      available: nonPrincipalCount > 0,
    },
    {
      when: "At delivery",
      title: "Governance receipt reference",
      detail: receipt
        ? `${receipt.scheme || "DSSE"} - key ${short(receipt.key_id || "-", 8, 6)}${receipt.inclusion_seq != null ? ` - inclusion log #${receipt.inclusion_seq}` : ""}. Receipt presence is not signature verification.`
        : "No governance receipt is available in this view. Delivery may still be in progress, or the receipt may not have been retained.",
      available: Boolean(receipt),
    },
  ];
  const available = points.filter((point) => point.available).length;
  const unavailable = points.length - available;

  return (
    <details className="mt-4 rounded-xl border border-border bg-surface-muted/20">
      <summary className="flex cursor-pointer list-none items-center gap-2 px-3 py-2 text-xs font-medium text-foreground-muted">
        <Icon name="seal" size={13} />
        Run evidence references
        <span className="ml-auto text-[10px]">
          {available} available · {unavailable} unavailable
        </span>
      </summary>
      <ol className="space-y-2 border-t border-border p-3">
        {points.map((point) => (
          <li key={point.title} className="flex gap-2.5">
            <span className="mt-0.5 inline-flex h-4 w-4 shrink-0 items-center justify-center rounded-full border border-border text-[9px] text-foreground-muted">
              {point.available ? "i" : "—"}
            </span>
            <div className="min-w-0">
              <p className="text-[11px] font-medium">
                {point.title}
                <span className="ml-2 rounded-full border border-border px-1.5 py-0.5 text-[9px] font-normal text-foreground-muted">
                  {point.when}
                </span>
              </p>
              <p className="text-[11px] leading-relaxed text-foreground-muted">{point.detail}</p>
            </div>
          </li>
        ))}
      </ol>
      <p className="border-t border-border px-3 py-2 text-[10px] leading-relaxed text-foreground-muted">
        Available means a reference is present in this view, not that its signature or the associated
        control has been verified. Unavailable means evidence is missing here; it does not establish
        success, failure, or the age of this run. Use the Auditor receipt-verification tools for cryptographic checks.
      </p>
    </details>
  );
}
