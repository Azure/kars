import type * as React from "react";
import { Icon } from "@/components/icon";
import type { AgentIdentity, Receipt } from "@/lib/types";
import { formatLastActivity, phaseKind, phaseTone } from "./activity";
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
            detail: `${agent.technicalName} · ${agent.model ?? "model not reported"} · ${agent.phase}`,
            failed: phaseKind(agent.phase) === "failed",
          }))}
        />
      </InspectorShell>
    );
  }
  if (selection.kind === "action") {
    const action = selection.action;
    return (
      <InspectorShell eyebrow="Recorded action" title={action.human}>
        <div className="flex flex-wrap gap-2 text-[10px] text-foreground-muted">
          <span className="rounded-full border border-border px-2 py-1">Agent {selection.agent.displayName}</span>
          <span className="rounded-full border border-border px-2 py-1">Round {action.round + 1}</span>
          <span className="rounded-full border border-border px-2 py-1">{action.ms} ms</span>
          <span className={`rounded-full border px-2 py-1 ${
            action.ok === false ? "border-danger/30 text-danger" : action.ok === true ? "border-signal/30 text-signal" : "border-border"
          }`}>
            {action.ok === false ? "Failed" : action.ok === true ? "Succeeded" : "Model round"}
          </span>
          <span className="rounded-full border border-border px-2 py-1">{formatLastActivity(action.ts)}</span>
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
          items={selection.actions.map((action) => ({
            title: action.human,
            detail: `${action.raw} · round ${action.round + 1} · ${action.ms} ms · ${formatLastActivity(action.ts)}`,
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

function AgentInspector({ agent }: { agent: AgentExecution }) {
  const latest = agent.actions.at(-1);
  return (
    <InspectorShell eyebrow={agent.isPrincipal ? "Principal agent" : "Specialist agent"} title={agent.displayName}>
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
        <Metric label="Retained rounds" value={agent.rounds} />
        <Metric label="Tool calls" value={agent.toolCalls} />
        <Metric label="Failures" value={agent.failures} danger={agent.failures > 0} />
        <Metric label="Latest activity" value={formatLastActivity(agent.lastActivity)} small />
      </div>
      <div className="mt-2 rounded-lg border border-border bg-surface px-3 py-2">
        <p className="text-[9px] font-medium uppercase tracking-wide text-foreground-muted">Latest action</p>
        <p className="mt-0.5 text-xs font-medium">{latest?.human ?? "No retained action yet"}</p>
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

export function ProofPoints({
  envelopeDigest,
  identity,
  subCount,
  receipt,
}: {
  envelopeDigest: string | null;
  identity: AgentIdentity | null;
  subCount: number;
  receipt: Receipt | null;
}) {
  const short = (value: string, head = 10, tail = 6) =>
    value.length > head + tail + 1
      ? `${value.slice(0, head)}...${value.slice(-tail)}`
      : value;
  const points = [
    {
      when: "At admission",
      title: "Trust envelope signed",
      detail: envelopeDigest
        ? `Digest ${short(envelopeDigest.replace(/^sha256:/, ""))} - tier, budget, tools, and reach were sealed before launch.`
        : "Tier, budget, tools, and reach are sealed into a signed envelope before launch.",
      proven: Boolean(envelopeDigest),
    },
    {
      when: "At registration",
      title: "Agent mesh identity (DID)",
      detail: identity?.did
        ? `${short(identity.did, 16, 8)} - signed mesh participant${identity.reputation_score != null ? `, reputation ${identity.reputation_score}` : ""}.`
        : "No per-run DID registration proof is attached to this retained view.",
      proven: Boolean(identity?.did),
    },
    {
      when: "At spawn",
      title: "Sub-agent attenuation enforced",
      detail:
        subCount > 0
          ? `${subCount} specialist${subCount === 1 ? "" : "s"} spawned after the controller verified each envelope was a strict subset of the principal's authority.`
          : "If the principal delegates, the controller rejects any sub-agent envelope that is not a strict authority subset.",
      proven: subCount > 0,
    },
    {
      when: "At delivery",
      title: "Governance receipt (DSSE)",
      detail: receipt
        ? `${receipt.scheme || "DSSE"} - key ${short(receipt.key_id || "-", 8, 6)}${receipt.inclusion_seq != null ? ` - inclusion log #${receipt.inclusion_seq}` : ""}.`
        : "No per-run DSSE receipt object is attached to this retained view.",
      proven: Boolean(receipt),
    },
  ];
  const verified = points.filter((point) => point.proven).length;
  const notRetained = points.length - verified;

  return (
    <details className="mt-4 rounded-xl border border-border bg-surface-muted/20">
      <summary className="flex cursor-pointer list-none items-center gap-2 px-3 py-2 text-xs font-medium text-foreground-muted">
        <Icon name="seal" size={13} />
        Cryptographic proofs and attestations
        <span className="ml-auto text-[10px]">
          {verified} verified · {notRetained} not retained
        </span>
      </summary>
      <ol className="space-y-2 border-t border-border p-3">
        {points.map((point) => (
          <li key={point.title} className="flex gap-2.5">
            <span className={`mt-0.5 inline-flex h-4 w-4 shrink-0 items-center justify-center rounded-full text-[9px] ${point.proven ? "bg-signal/15 text-signal" : "border border-dashed border-border text-foreground-muted"}`}>
              {point.proven ? "ok" : "n/a"}
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
        “Verified” means the exact per-run proof object is attached here. “Not retained” means this
        archived run predates that retained evidence surface; it is not counted as cryptographic proof,
        even when the platform control was enforced.
      </p>
    </details>
  );
}
