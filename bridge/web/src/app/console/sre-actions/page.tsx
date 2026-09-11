// kars Bridge Operator Console — SRE Actions. The kars-sre agent's
// self-remediation proposal surface: it diagnoses a workload incident and
// proposes ONE typed fix (KarsSREAction, closed action set); an operator
// approves or rejects here. On approval the controller mints a narrowly-
// scoped one-shot token, executes, tears the binding down, and records the
// real outcome (Applied/Recovered/Failed) — the Bridge never executes the
// remediation itself, only records the human decision.

import { PageHeader, Section, Badge } from "@/components/ui";
import { HonestState } from "@/components/honest-state";
import { Icon } from "@/components/icon";
import { listSreActions } from "@/lib/bff";
import { SreActionDecision } from "../sre-action-decision";
import type { SreAction } from "@/lib/types";

export const dynamic = "force-dynamic";

type Tone = "ok" | "warn" | "danger" | "info" | "muted" | "accent";

function phaseTone(phase: string): Tone {
  switch (phase) {
    case "Applied":
    case "Recovered":
      return "ok";
    case "Failed":
    case "Degraded":
      return "danger";
    case "Approved":
      return "info";
    case "Rejected":
    case "Expired":
      return "muted";
    default:
      return "warn"; // Proposed — awaiting operator decision
  }
}

export default async function SreActionsPage() {
  let actions: SreAction[] = [];
  let error = false;
  try {
    actions = await listSreActions();
  } catch {
    error = true;
  }

  const pending = actions.filter((a) => a.actionable).length;

  return (
    <div className="space-y-6">
      <PageHeader
        eyebrow="Operator Console"
        title="SRE Actions"
        lead="Self-remediation proposals from the kars-sre agent. Each is ONE typed, scoped fix (delete a stuck ResourceQuota, roll back an image, scale/restart a workload) with a rationale — you approve or reject. On approval the controller mints a one-shot, narrowly-scoped token, executes, tears it down, and records the real outcome."
      />

      {error ? (
        <HonestState variant="not_wired" title="Cluster unreachable" detail="The Bridge backend can't reach the cluster right now." />
      ) : actions.length === 0 ? (
        <HonestState
          variant="empty"
          title="No SRE proposals"
          detail="If the kars-sre agent (runtimes/hermes plugin) is deployed and diagnosing the cluster, its remediation proposals will appear here as KarsSREAction objects, Pending, until you approve or reject. None have been raised yet — this is a legitimate idle state, not a missing integration."
        />
      ) : (
        <Section
          title="Remediation proposals"
          action={
            <Badge tone={pending > 0 ? "warn" : "muted"} dot={pending > 0}>
              {pending > 0 ? `${pending} pending` : `${actions.length} total`}
            </Badge>
          }
        >
          <div className="space-y-3">
            {actions.map((a) => (
              <div key={`${a.namespace}/${a.name}`} className="rounded-lg border border-border bg-surface p-4">
                <div className="flex flex-wrap items-start justify-between gap-3">
                  <div>
                    <p className="flex items-center gap-1.5 text-sm font-semibold">
                      <Icon name="wrench" className="h-3.5 w-3.5 text-foreground-muted" /> {a.action_type}
                      {a.target_namespace && (
                        <span className="font-normal text-foreground-muted">
                          &nbsp;→ {a.target_namespace}{a.target_name ? `/${a.target_name}` : ""}
                        </span>
                      )}
                    </p>
                    {a.diagnosis && <p className="mt-1 text-xs text-foreground-muted">{a.diagnosis}</p>}
                  </div>
                  <div className="flex shrink-0 items-center gap-2">
                    <Badge tone={phaseTone(a.approval_state === "Pending" ? "Proposed" : a.phase)}>
                      {a.approval_state === "Pending" ? "Awaiting decision" : a.phase}
                    </Badge>
                  </div>
                </div>
                {a.rationale && (
                  <p className="mt-2 rounded-md bg-surface-muted/40 p-2 text-xs text-foreground-muted">{a.rationale}</p>
                )}
                <dl className="mt-3 grid grid-cols-2 gap-2 text-xs sm:grid-cols-4">
                  <div><dt className="text-foreground-muted">Approval</dt><dd className="font-medium">{a.approval_state}</dd></div>
                  <div><dt className="text-foreground-muted">TTL</dt><dd className="font-medium">{a.ttl_minutes ?? 15}m</dd></div>
                  <div><dt className="text-foreground-muted">Created</dt><dd className="font-medium">{a.created_at ? new Date(a.created_at).toLocaleString() : "—"}</dd></div>
                  <div><dt className="text-foreground-muted">Applied</dt><dd className="font-medium">{a.applied_at ? new Date(a.applied_at).toLocaleString() : "—"}</dd></div>
                </dl>
                {a.approval_note && (
                  <p className="mt-2 text-xs italic text-foreground-muted">&ldquo;{a.approval_note}&rdquo;</p>
                )}
                <div className="mt-3">
                  <SreActionDecision action={a} />
                </div>
              </div>
            ))}
          </div>
        </Section>
      )}
    </div>
  );
}
