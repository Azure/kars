"use client";

// kars Bridge Operator Console — per-item delete/revoke control. A two-step
// confirm (click → "Confirm?") so a governance object is never removed by a
// stray click. The delete is a real DELETE against the operator API (RBAC +
// finalizer enforced server-side); for an EgressApproval, delete IS the revoke.

import { useActionState, useState } from "react";
import { deleteGovernanceAction, type GovState } from "./governance-actions";

const init: GovState = { error: null, ok: null };

export function DeleteResource({
  kind,
  name,
  label,
  verb = "Remove",
}: {
  kind: "ToolPolicy" | "McpServer" | "KarsSkill" | "KarsProfile" | "EgressApproval" | "InferencePolicy";
  name: string;
  /** Human noun for the confirm copy, e.g. "tool policy". */
  label: string;
  /** Button verb — "Remove" for CRUD, "Revoke" for egress grants. */
  verb?: "Remove" | "Revoke";
}) {
  const [state, action, pending] = useActionState(deleteGovernanceAction, init);
  const [confirming, setConfirming] = useState(false);

  if (state.ok) {
    // The list is revalidated server-side; show a brief tombstone until refresh.
    return <span className="text-[11px] text-foreground-muted">{verb}d — refreshing…</span>;
  }

  if (!confirming) {
    return (
      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={() => setConfirming(true)}
          className="rounded-md border border-border px-2 py-1 text-[11px] font-medium text-foreground-muted hover:border-danger/40 hover:text-danger"
        >
          {verb}
        </button>
        {state.error && <span className="text-[11px] text-danger">{state.error}</span>}
      </div>
    );
  }

  return (
    <form action={action} className="flex items-center gap-2">
      <input type="hidden" name="kind" value={kind} />
      <input type="hidden" name="name" value={name} />
      <span className="text-[11px] text-foreground-muted">{verb} {label} “{name}”?</span>
      <button
        type="submit"
        disabled={pending}
        className="rounded-md border border-danger/40 bg-danger/10 px-2 py-1 text-[11px] font-semibold text-danger disabled:opacity-50"
      >
        {pending ? `${verb.slice(0, -1)}ing…` : `Yes, ${verb.toLowerCase()}`}
      </button>
      <button
        type="button"
        onClick={() => setConfirming(false)}
        className="text-[11px] text-foreground-muted hover:text-foreground"
      >
        Cancel
      </button>
    </form>
  );
}
