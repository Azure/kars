// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge Operator Console — SRE Actions. The kars-sre agent's
// self-remediation proposal surface: it diagnoses a workload incident and
// proposes ONE typed fix (KarsSREAction, closed action set); an operator
// approves or rejects here. On approval the controller mints a narrowly-
// scoped short-lived token, claims a single execution attempt, tears the binding down, and records the
// real outcome (Applied/Recovered/Failed) — the Bridge never executes the
// remediation itself, only records the human decision.

import { PageHeader, Section, Badge } from "@/components/ui";
import { HonestState } from "@/components/honest-state";
import { Icon } from "@/components/icon";
import { getSreReport, listSandboxes, listSreActions } from "@/lib/bff";
import type { SreReport } from "@/lib/types";
import { SreActionDecision } from "../sre-action-decision";

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

function HealthReport({ result }: { result: SreReport | null }) {
  const state = result?.state ?? "unavailable";
  const titles = {
    observed: "Automatic SRE report received",
    stale: "SRE report stale — current health unknown",
    missing: "No persisted SRE report yet",
    revoked: "SRE report source retired or disabled",
    replaced: "SRE report source identity changed",
    invalid: "SRE report could not be validated",
    unavailable: "SRE report unavailable",
  };
  const latest = state === "observed" || state === "stale" ? result?.latest : null;
  return (
    <Section title="Latest automatic health report">
      <p className="text-sm font-semibold">{titles[state] ?? titles.unavailable}</p>
      <p className="mt-1 text-xs text-foreground-muted">
        Advisory agent output, not verified cluster health. Failed reads and unknown coverage remain unknown.
        Reports do not approve remedies or grant inference capacity; proposals below require a separate decision.
      </p>
      {latest ? (
        <div className="mt-3 space-y-3">
          <Badge tone={latest.report.turn_status === "finished" ? "info" : "warn"}>
            {latest.report.turn_status === "finished" ? "Finished turn — health unverified" : `Turn ${latest.report.turn_status} — diagnostic coverage incomplete`}
          </Badge>
          <p className="text-xs text-foreground-muted">
            Completed {new Date(latest.report.finished_at_ms).toISOString()} · Persisted {new Date(latest.accepted_at_ms).toISOString()}
          </p>
          {latest.report.truncated && <p className="text-xs text-foreground-muted">Report excerpt truncated; omitted evidence is not verified.</p>}
          <pre className="max-h-96 overflow-auto whitespace-pre-wrap break-words rounded-md border border-border bg-surface p-3 text-xs">{latest.report.text}</pre>
        </div>
      ) : <p className="mt-2 text-xs text-foreground-muted">No report from a verified current source is available. This is not an all-clear or an empty proposal history.</p>}
    </Section>
  );
}

export default async function SreActionsPage() {
  const [actionsResult, inventoryResult, reportResult] = await Promise.allSettled([
    listSreActions(),
    listSandboxes(),
    getSreReport(),
  ]);
  const actions = actionsResult.status === "fulfilled" ? actionsResult.value : null;
  const sre = inventoryResult.status === "fulfilled"
    ? inventoryResult.value.find((sandbox) => sandbox.name === "sre")
    : undefined;
  const pending = actions?.filter((a) => a.actionable).length ?? 0;

  return (
    <div className="space-y-6">
      <PageHeader
        eyebrow="Operator Console"
        title="SRE Actions"
        lead="Self-remediation proposals from the kars-sre agent. Each is ONE typed, scoped fix (delete a stuck ResourceQuota, roll back an image, scale/restart a workload) with a rationale — review the persisted action parameters before approving or rejecting. On approval the controller claims a single execution attempt, uses a short-lived, narrowly scoped token, removes the temporary permissions, and records the real outcome."
      />

      {inventoryResult.status === "rejected" ? (
        <HonestState
          variant="not_wired"
          title="SRE status unavailable"
          detail="Sandbox inventory could not be read. A proposal count does not confirm that the SRE agent is installed or diagnosing the cluster."
        />
      ) : !sre ? (
        <HonestState
          variant="not_wired"
          title="SRE agent not installed"
          detail="No sre sandbox was found. Install and enroll kars-SRE to enable diagnosis; retained proposals can still be reviewed below."
        />
      ) : null}

      <HealthReport result={reportResult.status === "fulfilled" ? reportResult.value : null} />

      {actions === null ? (
        <HonestState variant="not_wired" title="SRE proposal history unavailable" detail="Remediation proposals could not be read. Refresh to retry; this is not an empty history." />
      ) : actions.length === 0 ? sre ? (
        <HonestState
          variant="empty"
          title="No SRE proposals"
          detail={`No remediation proposals have been recorded. The SRE sandbox reports ${sre.phase ?? "Unknown"}; this alone does not verify diagnostic tool access or successful model calls.`}
        />
      ) : null : (
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
                <p className="mt-2 break-all text-xs text-foreground-muted">Proposal: {a.namespace}/{a.name}</p>
                {a.rationale && (
                  <p className="mt-2 rounded-md bg-surface-muted/40 p-2 text-xs text-foreground-muted">{a.rationale}</p>
                )}
                <div className="mt-3">
                  <p className="text-xs font-semibold">Proposed action parameters</p>
                  <pre className="mt-1 max-h-64 overflow-auto whitespace-pre-wrap break-words rounded-md border border-border bg-surface-muted/40 p-3 text-xs">{a.params === undefined ? "Action parameters unavailable" : JSON.stringify(a.params, null, 2)}</pre>
                </div>
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
