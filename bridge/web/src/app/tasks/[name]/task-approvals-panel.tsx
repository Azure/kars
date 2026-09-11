// kars Bridge — task-scoped approvals panel. Shows the human decisions gating
// this task (the steering surface, in the task's own context), with inline
// approve/deny for any still pending.

import { ApprovalDecision } from "@/components/approval-decision";
import { ApprovalPhaseBadge, actionLabel } from "@/components/approval-phase-badge";
import { TIER_LABELS, type Approval } from "@/lib/types";

export function TaskApprovalsPanel({
  approvals,
  decider,
  authWired,
}: {
  approvals: Approval[];
  decider: string;
  authWired: boolean;
}) {
  if (approvals.length === 0) return null;
  const pendingCount = approvals.filter((a) => a.actionable).length;

  return (
    <section
      aria-labelledby="approvals-heading"
      className="overflow-hidden rounded-xl border border-border bg-surface"
    >
      <div className="border-b border-border px-6 py-4">
        <h2 id="approvals-heading" className="text-sm font-semibold">
          Approvals
          {pendingCount > 0 && (
            <span className="ml-2 rounded-full bg-warning/15 px-2 py-0.5 text-xs font-medium text-warning">
              {pendingCount} awaiting
            </span>
          )}
        </h2>
        <p className="mt-0.5 text-xs text-foreground-muted">
          Human decisions gating this task. Each is bound to the envelope it was
          requested under and recorded in the Governance Receipt.
        </p>
      </div>
      <ul className="divide-y divide-border">
        {approvals.map((a) => (
          <li key={a.name} className="px-6 py-4">
            <div className="flex items-start justify-between gap-4">
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <span className="rounded border border-border bg-surface-muted px-1.5 py-0.5 text-xs font-medium text-foreground-muted">
                    {actionLabel(a.action_kind)}
                  </span>
                  {a.requested_tier != null && (
                    <span className="text-xs text-foreground-muted">
                      → Tier {a.requested_tier} ·{" "}
                      {TIER_LABELS[a.requested_tier] ?? "?"}
                    </span>
                  )}
                </div>
                <p className="mt-1.5 text-sm font-medium">{a.summary}</p>
                {a.detail && (
                  <p className="mt-0.5 text-xs text-foreground-muted">{a.detail}</p>
                )}
              </div>
              <ApprovalPhaseBadge phase={a.phase} />
            </div>
            {a.actionable ? (
              <div className="mt-3">
                <ApprovalDecision
                  name={a.name}
                  decider={decider}
                  authWired={authWired}
                  resourceVersion={a.resource_version}
                  boundEnvelopeDigest={a.bound_envelope_digest}
                  compact
                  requireReason={a.action_kind === "clarification"}
                />
              </div>
            ) : (
              <p className="mt-2 text-xs text-foreground-muted">
                {a.decider
                  ? `${a.phase} by ${a.decider}${a.decided_at ? ` · ${a.decided_at}` : ""}`
                  : a.phase}
              </p>
            )}
          </li>
        ))}
      </ul>
    </section>
  );
}
