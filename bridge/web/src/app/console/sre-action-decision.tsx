"use client";

// kars-SRE remediation-proposal approve/reject control. Mirrors SkillApproval:
// a client component driving the decideSreActionAction server action so the
// approve/reject buttons show pending state and inline errors.

import { useActionState } from "react";
import { decideSreActionAction } from "./governance-actions";
import type { GovState } from "./governance-actions";
import type { SreAction } from "@/lib/types";

const init: GovState = { error: null, ok: null };

export function SreActionDecision({ action: sreAction }: { action: SreAction }) {
  const [state, formAction, pending] = useActionState(decideSreActionAction, init);

  if (state.ok) {
    return <span className="text-[11px] text-signal">{state.ok} refreshing…</span>;
  }

  if (!sreAction.actionable) {
    return null;
  }

  return (
    <div className="flex flex-wrap items-center gap-2">
      <form action={formAction}>
        <input type="hidden" name="ns" value={sreAction.namespace} />
        <input type="hidden" name="name" value={sreAction.name} />
        <input type="hidden" name="action" value="approve" />
        <button
          type="submit"
          disabled={pending}
          className="rounded-md bg-signal px-2.5 py-1 text-[11px] font-semibold text-white hover:bg-signal/90 disabled:opacity-50"
        >
          {pending ? "Deciding…" : "Approve"}
        </button>
      </form>
      <form action={formAction}>
        <input type="hidden" name="ns" value={sreAction.namespace} />
        <input type="hidden" name="name" value={sreAction.name} />
        <input type="hidden" name="action" value="reject" />
        <button
          type="submit"
          disabled={pending}
          className="rounded-md border border-border px-2.5 py-1 text-[11px] font-medium text-foreground-muted hover:border-danger/40 hover:text-danger disabled:opacity-50"
        >
          {pending ? "Deciding…" : "Reject"}
        </button>
      </form>
      {state.error && <span className="text-[11px] text-danger">{state.error}</span>}
    </div>
  );
}
