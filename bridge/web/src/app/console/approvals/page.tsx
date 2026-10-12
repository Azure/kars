// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge Operator Console — Approvals. Operator-side governance gates:
// temporary egress widenings (EgressApproval) the platform team grants, and a
// pointer to fleet-wide steering decisions. Real reads.

import { PageHeader, Section, Badge } from "@/components/ui";
import { HonestState } from "@/components/honest-state";
import { Icon } from "@/components/icon";
import { DeleteResource } from "../delete-resource";
import { listApprovals, listEgress } from "@/lib/bff";
import { defaultNamespace } from "@/lib/config";
import { currentPrincipal } from "@/lib/session";
import { ApprovalDecision } from "@/components/approval-decision";
import { ApprovalPhaseBadge, actionLabel } from "@/components/approval-phase-badge";
import { ConsoleLoadError } from "@/components/console-load-error";
import type { Approval, EgressApproval } from "@/lib/types";

export const dynamic = "force-dynamic";

type Tone = "ok" | "warn" | "danger" | "info" | "muted" | "accent";

function phaseTone(p: string | null): Tone {
  switch (p) {
    case "Active":
      return "ok";
    case "Pending":
      return "warn";
    default:
      return "muted";
  }
}

export default async function ConsoleApprovals() {
  const principal = await currentPrincipal();
  let egress: EgressApproval[] = [];
  let steering: Approval[] = [];
  try {
    [egress, steering] = await Promise.all([
      listEgress(),
      listApprovals(defaultNamespace(), { scopeAll: true }),
    ]);
    steering = steering.filter((approval) => approval.action_kind !== "clarification");
  } catch (error) {
    return <ConsoleLoadError title="Approvals" error={error}
      detail="The approval queues could not be verified. This does not mean no requests are pending. Decisions are unavailable until the read succeeds." />;
  }

  const pending = egress.filter((e) => e.phase === "Pending").length;

  return (
    <div className="space-y-6">
      <PageHeader
        eyebrow="Operator Console"
        title="Approvals"
        lead="Fleet-wide workload decisions and temporary egress grants. Review authority requests and checkpoints, or revoke an active network widening. Agent clarification questions remain in the owning user's Workspace inbox."
      />

      {/* Two approval planes — the operator asked how this relates to the inbox. */}
      <section className="grid gap-3 sm:grid-cols-2">
        <div className="rounded-xl border border-signal/25 bg-signal/[0.04] p-4">
          <p className="flex items-center gap-2 text-sm font-semibold"><span aria-hidden><Icon name="seal" size={16} /></span> This page — operator decisions</p>
          <p className="mt-1 text-xs text-foreground-muted">
            Review workload authority requests and Team checkpoints across the fleet. Temporary
            <strong> egress widenings</strong> (EgressApproval) are listed separately below; revoking
            one asks the controller to restore that sandbox&rsquo;s baseline allowlist.
          </p>
        </div>
        <div className="rounded-xl border border-accent/25 bg-accent/[0.04] p-4">
          <p className="flex items-center gap-2 text-sm font-semibold"><span aria-hidden><Icon name="message" size={16} /></span> Workspace inbox — mission plane</p>
          <p className="mt-1 text-xs text-foreground-muted">
            Task-givers see their own requests and answer agent clarification questions in the
            <strong> Workspace inbox</strong>. Authority expansion and checkpoint decisions require
            an operator or admin; clarification questions are excluded from this fleet view.
          </p>
        </div>
      </section>

      <Section
        title="Workload decisions"
        action={
          <Badge tone={steering.some((approval) => approval.actionable) ? "warn" : "muted"} dot={steering.some((approval) => approval.actionable)}>
            {steering.filter((approval) => approval.actionable).length} pending
          </Badge>
        }
      >
        {steering.length === 0 ? (
          <HonestState
            variant="empty"
            title="No workload decisions"
            detail="Mission and Team authority requests and checkpoints appear here. Clarification questions remain in the owning user's Workspace inbox."
          />
        ) : (
          <ul className="divide-y divide-border overflow-hidden rounded-lg border border-border">
            {steering.map((approval) => (
              <li key={approval.name} className="px-4 py-4">
                <div className="flex items-start justify-between gap-4">
                  <div className="min-w-0">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="rounded border border-border bg-surface-muted px-1.5 py-0.5 text-xs font-medium text-foreground-muted">
                        {actionLabel(approval.action_kind)}
                      </span>
                      <span className="break-all font-mono text-[11px] text-foreground-muted">{approval.task}</span>
                    </div>
                    <p className="mt-1.5 text-sm font-medium">{approval.summary}</p>
                    {approval.detail && (
                      <details className="mt-1 text-xs text-foreground-muted">
                        <summary className="cursor-pointer">Request details</summary>
                        <p className="mt-2 whitespace-pre-wrap break-words">{approval.detail}</p>
                      </details>
                    )}
                  </div>
                  <ApprovalPhaseBadge phase={approval.phase} />
                </div>
                {approval.actionable && (
                  <div className="mt-3">
                    <ApprovalDecision
                      name={approval.name}
                      decider={principal.name}
                      authWired
                      resourceVersion={approval.resource_version}
                      boundEnvelopeDigest={approval.bound_envelope_digest}
                      compact
                      requireDenyReason={approval.action_kind === "checkpoint" && !!approval.team && !!approval.milestone}
                    />
                  </div>
                )}
              </li>
            ))}
          </ul>
        )}
      </Section>

      {/* How an egress request becomes a grant. */}
      <div className="rounded-xl border border-border bg-surface-muted/30 p-4 text-xs text-foreground-muted">
        <p className="font-medium text-foreground">How an egress approval flows</p>
        <p className="mt-1.5">
          Agent reaches a novel domain → the router denies it and records the request → in
          <strong className="text-foreground"> learning</strong> mode the operator reviews observed
          domains and pins an allowlist; in <strong className="text-foreground">strict</strong> mode a
          temporary <strong className="text-foreground">EgressApproval</strong> is granted (here) for a
          bounded window → the controller widens that sandbox&rsquo;s allowlist → it expires or you
          revoke it, and the allowlist reconciles back to baseline.
        </p>
      </div>

      <Section
        title="Temporary egress grants"
        action={
          <Badge tone={pending > 0 ? "warn" : "muted"} dot={pending > 0}>
            {pending > 0 ? `${pending} pending` : `${egress.length} total`}
          </Badge>
        }
      >
        {egress.length === 0 ? (
          <div className="space-y-4">
            <HonestState
              variant="empty"
              title="No temporary egress grants"
              detail="Sandboxes run on their signed baseline allowlist. Active temporary widenings appear here, where you can revoke them; the controller reconciles the allowlist back to baseline on revoke."
            />
            {/* Sample (disabled) grant so the action model is visible even when
                the queue is empty (audit f38). */}
            <div>
              <p className="mb-2 text-[11px] font-semibold uppercase tracking-wide text-foreground-muted">Example — what a grant looks like</p>
              <div className="rounded-lg border border-dashed border-border bg-surface-muted/30 p-4 opacity-80">
                <div className="flex items-center justify-between gap-3">
                  <span className="font-mono text-sm font-medium text-foreground-muted">research-run-1783•••</span>
                  <Badge tone="muted">Example — not a real grant</Badge>
                </div>
                <div className="mt-2 flex flex-wrap gap-1">
                  <span className="rounded-full border border-border bg-surface px-2 py-0.5 font-mono text-[11px]">api.github.com:443</span>
                  <span className="rounded-full border border-border bg-surface px-2 py-0.5 font-mono text-[11px]">raw.githubusercontent.com:443</span>
                </div>
                <div className="mt-3 flex items-center justify-between">
                  <p className="text-[11px] text-foreground-muted">A time-boxed widening the agent requested and you approved. It sits above the signed baseline and expires or is revoked back to baseline.</p>
                  <button type="button" disabled className="ml-3 shrink-0 rounded-lg border border-border px-3 py-1.5 text-xs font-medium text-foreground-muted opacity-60" title="This is an example — revoke appears on real grants">Revoke</button>
                </div>
              </div>
            </div>
          </div>
        ) : (
          <ul className="space-y-3">
            {egress.map((e) => (
              <li key={`${e.namespace}/${e.name}`} className="rounded-lg border border-border bg-surface p-4">
                <div className="flex items-center justify-between gap-3">
                  <span className="font-mono text-sm font-medium">{e.sandbox ?? e.name}</span>
                  <Badge tone={phaseTone(e.phase)} dot>
                    {e.phase ?? "Unknown"}
                  </Badge>
                </div>
                {e.hosts.length > 0 && (
                  <div className="mt-2 flex flex-wrap gap-1">
                    {e.hosts.map((h) => (
                      <span key={h} className="rounded bg-surface-muted px-1.5 py-0.5 font-mono text-[11px] text-foreground-muted">{h}</span>
                    ))}
                  </div>
                )}
                {e.reason && <p className="mt-2 text-xs text-foreground-muted">{e.reason}</p>}
                {e.expires_at && (
                  <p className="mt-0.5 text-xs text-foreground-muted">expires {new Date(e.expires_at).toLocaleString()}</p>
                )}
                <div className="mt-2">
                  <DeleteResource kind="EgressApproval" name={e.name} label="egress grant" verb="Revoke" />
                </div>
              </li>
            ))}
          </ul>
        )}
      </Section>

      <p className="px-1 text-xs text-foreground-muted">
        Decisions are bound to the exact request and envelope shown for review. A changed or already
        decided request must be reloaded before another decision can be submitted.
      </p>
    </div>
  );
}
