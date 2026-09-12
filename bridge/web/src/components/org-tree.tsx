"use client";

// kars Bridge — a real org chart. Nodes, drawn reporting-line edges, and live
// per-node state. The reporting line IS the trust boundary, so we actually draw
// it: a principal on top, connectors down to each member, using the classic CSS
// connector technique (pure borders) so the lines never break on wrap — the row
// scrolls horizontally instead of collapsing into a stack. Used for both team
// and mission orgs.

import Link from "next/link";
import { useState } from "react";
import { ViewportPortal } from "@/components/viewport-portal";

export type OrgStatus = "principal" | "running" | "verified" | "pending" | "degraded";

export type OrgNode = {
  id: string;
  title: string;
  /** Secondary line, e.g. "Tier 2 · Shared" or "Principal · team lead". */
  role?: string;
  chips?: { label: string; tone?: "muted" | "signal" | "accent" | "danger" | "ok" }[];
  status?: OrgStatus;
  href?: string;
  /** Optional short prompt/description shown under the title. */
  detail?: string;
};

const STATUS_DOT: Record<OrgStatus, string> = {
  principal: "bg-signal",
  running: "bg-ok kb-pulse",
  verified: "bg-ok",
  pending: "bg-foreground-muted",
  degraded: "bg-danger",
};

const STATUS_LABEL: Record<OrgStatus, string> = {
  principal: "Principal",
  running: "Running",
  verified: "Verified",
  pending: "Launch-verified",
  degraded: "Rejected",
};

const CHIP_TONE: Record<string, string> = {
  muted: "border-border bg-surface-muted text-foreground-muted",
  signal: "border-signal/30 bg-signal/10 text-signal",
  accent: "border-accent/30 bg-accent/10 text-accent",
  danger: "border-danger/30 bg-danger/10 text-danger",
  ok: "border-ok/30 bg-ok/10 text-ok",
};

function Card({ node }: { node: OrgNode }) {
  const status = node.status ?? "pending";
  const principal = status === "principal";
  const inner = (
    <div
      className={`w-52 rounded-xl border px-3.5 py-2.5 text-left shadow-sm transition sm:w-56 ${
        principal
          ? "border-signal/50 bg-signal/[0.07]"
          : "border-border bg-surface hover:-translate-y-0.5 hover:border-signal/40 hover:shadow-md"
      }`}
    >
      <div className="flex items-start justify-between gap-2">
        <p className="min-w-0 line-clamp-2 break-words text-sm font-semibold leading-tight" title={node.title}>{node.title}</p>
        <span
          className={`inline-flex shrink-0 items-center gap-1 self-start whitespace-nowrap rounded-full border px-1.5 py-0.5 text-[9px] font-medium ${
            status === "degraded"
              ? "border-danger/30 bg-danger/10 text-danger"
              : principal
                ? "border-signal/30 bg-signal/10 text-signal"
                : status === "verified" || status === "running"
                  ? "border-ok/30 bg-ok/10 text-ok"
                  : "border-border bg-surface-muted text-foreground-muted"
          }`}
          title={STATUS_LABEL[status]}
        >
          <span className={`h-1.5 w-1.5 rounded-full ${STATUS_DOT[status]}`} aria-hidden />
          {STATUS_LABEL[status]}
        </span>
      </div>
      {node.role && <p className="mt-0.5 text-[11px] text-foreground-muted">{node.role}</p>}
      {node.detail && <p className="mt-1 line-clamp-2 text-[11px] text-foreground-muted">{node.detail}</p>}
      {node.chips && node.chips.length > 0 && (
        <div className="mt-2 flex flex-wrap gap-1">
          {node.chips.map((c, i) => (
            <span key={i} className={`rounded border px-1.5 py-0.5 text-[10px] font-medium ${CHIP_TONE[c.tone ?? "muted"]}`}>
              {c.label}
            </span>
          ))}
        </div>
      )}
    </div>
  );
  return node.href ? (
    <Link href={node.href} className="block" title={node.title}>
      {inner}
    </Link>
  ) : (
    inner
  );
}

export function OrgTree({
  principal,
  members,
  footer,
}: {
  principal: OrgNode;
  members: OrgNode[];
  /** Optional trailing node (e.g. an "+ Add member" affordance) drawn as a leaf. */
  footer?: React.ReactNode;
}) {
  const [expanded, setExpanded] = useState(false);
  return (
    <>
      <div className="mb-2 flex justify-end">
        <button
          type="button"
          onClick={() => setExpanded(true)}
          className="rounded-md border border-border bg-surface px-2.5 py-1 text-[11px] font-medium text-foreground-muted hover:border-signal/40 hover:text-foreground"
        >
          Expand org chart
        </button>
      </div>
      <TreeCanvas principal={principal} members={members} footer={footer} />
      {expanded && (
        <ViewportPortal onClose={() => setExpanded(false)}>
          <div
            role="dialog"
            aria-modal="true"
            aria-label="Expanded team org chart"
            className="fixed inset-0 z-[100] flex flex-col overflow-hidden bg-surface"
          >
            <div className="flex items-center justify-between border-b border-border px-5 py-3">
              <div>
                <p className="text-sm font-semibold">Team org chart</p>
                <p className="text-xs text-foreground-muted">Full-screen reporting and trust-boundary view</p>
              </div>
              <button
                type="button"
                onClick={() => setExpanded(false)}
                className="rounded-lg border border-border px-3 py-1.5 text-xs font-medium hover:bg-surface-muted"
              >
                Close
              </button>
            </div>
            <div className="min-h-0 flex-1 overflow-auto p-6">
              <TreeCanvas principal={principal} members={members} footer={footer} />
            </div>
          </div>
        </ViewportPortal>
      )}
    </>
  );
}

function TreeCanvas({
  principal,
  members,
  footer,
}: {
  principal: OrgNode;
  members: OrgNode[];
  footer?: React.ReactNode;
}) {
  const leaves = members.length + (footer ? 1 : 0);
  return (
    <div className="kb-orgtree overflow-x-auto pb-2">
      <ul>
        <li>
          <div className="kb-orgtree-node">
            <Card node={{ ...principal, status: "principal" }} />
          </div>
          {leaves > 0 && (
            <ul>
              {members.map((m) => (
                <li key={m.id}>
                  <div className="kb-orgtree-node">
                    <Card node={m} />
                  </div>
                </li>
              ))}
              {footer && (
                <li>
                  <div className="kb-orgtree-node">{footer}</div>
                </li>
              )}
            </ul>
          )}
        </li>
      </ul>
    </div>
  );
}
