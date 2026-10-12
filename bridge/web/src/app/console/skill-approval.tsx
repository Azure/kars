// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

// Independent manual review, bound to the exact package displayed to the reviewer.

import { useActionState } from "react";
import Link from "next/link";
import { Icon } from "@/components/icon";
import { reviewSkillAction, type GovState } from "./governance-actions";
import type { SkillSummary } from "@/lib/types";

const init: GovState = { error: null, ok: null };

export function SkillApproval({ skill, inspected = false }: { skill: SkillSummary; inspected?: boolean }) {
  const [state, action, pending] = useActionState(reviewSkillAction, init);
  const enabled = skill.validated && inspected;

  return (
    <div className="flex flex-wrap items-center gap-2">
      {!inspected && <Link href={`/console/capabilities/skills/${encodeURIComponent(skill.name)}`} className="text-xs text-signal underline">Review package</Link>}
      {skill.usable ? (
        <>
          <span className="inline-flex items-center gap-1 rounded-md bg-signal/10 px-2 py-0.5 text-[11px] font-medium text-signal">
            <Icon name="check" size={11} /> content-bound approval
          </span>
          <form action={action}>
            <input type="hidden" name="name" value={skill.name} />
            <input type="hidden" name="uid" value={skill.uid ?? ""} />
            <input type="hidden" name="action" value="revoke" />
            <button type="submit" disabled={pending} className="rounded-md border border-border px-2 py-1 text-[11px] font-medium text-foreground-muted hover:border-danger/40 hover:text-danger disabled:opacity-50">
              {pending ? "Revoking…" : "Revoke"}
            </button>
          </form>
        </>
      ) : skill.review === "approved" ? (
        <>
          <span className="inline-flex items-center gap-1 rounded-md bg-warning/10 px-2 py-0.5 text-[11px] font-medium text-warning">
            <Icon name="warning" size={12} className="inline mr-0.5" /> approval no longer current — re-review
          </span>
          <ApproveButton skill={skill} action={action} pending={pending} enabled={enabled} label="Re-approve this version" />
        </>
      ) : (
        <>
          <span className="inline-flex items-center gap-1 rounded-md bg-surface-muted px-2 py-0.5 text-[11px] font-medium text-foreground-muted">
            ● pending review
          </span>
          <ApproveButton skill={skill} action={action} pending={pending} enabled={enabled} />
        </>
      )}
      {!skill.validated && <span className="text-[11px] text-foreground-muted">{skill.validation_detail ?? "Awaiting current content validation"}</span>}
      {state.ok && <span role="status" className="text-[11px] text-signal">{state.ok}</span>}
      {state.error && <span role="alert" className="text-[11px] text-danger">{state.error}</span>}
    </div>
  );
}

function ApproveButton({ skill, action, pending, enabled, label }: { skill: SkillSummary; action: (fd: FormData) => void; pending: boolean; enabled: boolean; label?: string }) {
  return (
    <form action={action}>
      <input type="hidden" name="name" value={skill.name} />
      <input type="hidden" name="uid" value={skill.uid ?? ""} />
      <input type="hidden" name="generation" value={skill.generation ?? ""} />
      <input type="hidden" name="version_digest" value={skill.version_digest ?? ""} />
      <input type="hidden" name="action" value="approve" />
      <button
        type="submit"
        disabled={pending || !enabled}
        title={enabled ? "Approve this inspected UID, generation and content" : "Open Review package and wait for current controller validation"}
        className="rounded-md border border-signal/40 bg-signal/10 px-2.5 py-1 text-[11px] font-semibold text-signal disabled:opacity-40"
      >
        {pending ? "Approving…" : (label ?? "Approve this version")}
      </button>
    </form>
  );
}
