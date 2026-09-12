"use client";

// Operator skill-admission control. The trust gate the user described:
// a user-uploaded skill is scanned by the controller (attestation), then an
// operator reviews and APPROVES it, which LOCKS the approval to the exact
// version digest — only then may users assign it. Any later change breaks the
// lock and returns it to review. Approve is disabled until the scan verifies.

import { useActionState } from "react";
import { Icon } from "@/components/icon";
import { reviewSkillAction, type GovState } from "./governance-actions";
import type { SkillSummary } from "@/lib/types";

const init: GovState = { error: null, ok: null };

export function SkillApproval({ skill }: { skill: SkillSummary }) {
  const [state, action, pending] = useActionState(reviewSkillAction, init);
  const scanned = skill.version_digest != null;
  // Attestation is OPTIONAL: the controller marks a skill "validated and grantable"
  // even with "no attestation declared" (attestation_verified === null). Approval is
  // only blocked when the scan EXPLICITLY failed (attestation_verified === false) —
  // matching the BFF gate. A null attestation must not deadlock approval.
  const attestationFailed = skill.attestation_verified === false;
  const enabled = scanned && !attestationFailed;
  // "Approved" annotation with no lock recorded is a half-approved state (e.g. set
  // out-of-band), NOT genuine drift. Only a lock that no longer matches the live
  // digest is real "changed since approval".
  const drifted =
    skill.review === "approved" && skill.locked_digest != null && !skill.usable;
  const approvedUnlocked =
    skill.review === "approved" && skill.locked_digest == null && !skill.usable;

  if (state.ok) {
    return <span className="text-[11px] text-signal">{state.ok} refreshing…</span>;
  }

  return (
    <div className="flex flex-wrap items-center gap-2">
      {skill.usable ? (
        <>
          <span className="inline-flex items-center gap-1 rounded-md bg-signal/10 px-2 py-0.5 text-[11px] font-medium text-signal">
            <Icon name="check" size={11} /> locked
          </span>
          <form action={action}>
            <input type="hidden" name="name" value={skill.name} />
            <input type="hidden" name="action" value="revoke" />
            <button type="submit" disabled={pending} className="rounded-md border border-border px-2 py-1 text-[11px] font-medium text-foreground-muted hover:border-danger/40 hover:text-danger disabled:opacity-50">
              {pending ? "Revoking…" : "Revoke"}
            </button>
          </form>
        </>
      ) : drifted ? (
        // Approved earlier but the skill changed since — lock is broken.
        <>
          <span className="inline-flex items-center gap-1 rounded-md bg-warning/10 px-2 py-0.5 text-[11px] font-medium text-warning">
            <Icon name="warning" size={12} className="inline mr-0.5" /> changed since approval — re-review
          </span>
          <ApproveButton name={skill.name} action={action} pending={pending} enabled={enabled} label="Re-approve & lock" />
        </>
      ) : approvedUnlocked ? (
        // Approved out-of-band without a recorded lock — lock it to make it usable.
        <>
          <span className="inline-flex items-center gap-1 rounded-md bg-warning/10 px-2 py-0.5 text-[11px] font-medium text-warning">
            ● approved — not locked
          </span>
          <ApproveButton name={skill.name} action={action} pending={pending} enabled={enabled} />
        </>
      ) : (
        <>
          <span className="inline-flex items-center gap-1 rounded-md bg-surface-muted px-2 py-0.5 text-[11px] font-medium text-foreground-muted">
            ● pending review
          </span>
          <ApproveButton name={skill.name} action={action} pending={pending} enabled={enabled} />
        </>
      )}
      {!scanned && <span className="text-[11px] text-foreground-muted">awaiting scan…</span>}
      {attestationFailed && <span className="text-[11px] text-warning">attestation failed — can’t approve</span>}
      {state.error && <span className="text-[11px] text-danger">{state.error}</span>}
    </div>
  );
}

function ApproveButton({ name, action, pending, enabled, label }: { name: string; action: (fd: FormData) => void; pending: boolean; enabled: boolean; label?: string }) {
  return (
    <form action={action}>
      <input type="hidden" name="name" value={name} />
      <input type="hidden" name="action" value="approve" />
      <button
        type="submit"
        disabled={pending || !enabled}
        title={enabled ? "Approve and lock to this version" : "The controller must scan this skill (and any declared attestation must pass) before it can be approved"}
        className="rounded-md border border-signal/40 bg-signal/10 px-2.5 py-1 text-[11px] font-semibold text-signal disabled:opacity-40"
      >
        {pending ? "Approving…" : (label ?? "Approve & lock")}
      </button>
    </form>
  );
}
