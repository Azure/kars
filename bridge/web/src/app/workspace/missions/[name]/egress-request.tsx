"use client";

// Mission "request website access" form. The agent's egress is default-deny;
// this is how a human grants a scoped, time-boxed exception — surfaced plainly
// so the elevation ask is visible, not buried.

import { useActionState } from "react";
import { requestEgressAction, type EgressState } from "./egress-actions";

const init: EgressState = { error: null, ok: null };

export function EgressRequest({ mission }: { mission: string }) {
  const [state, action, pending] = useActionState(requestEgressAction, init);
  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <h2 className="text-sm font-semibold">Website access</h2>
      <p className="mt-0.5 text-xs text-foreground-muted">
        The agent can reach only the model path by default. Grant a scoped, time-boxed exception —
        it opens only after approval and expires automatically.
      </p>
      <form action={action} className="mt-4 space-y-3">
        <input type="hidden" name="mission" value={mission} />
        <div className="flex flex-wrap gap-2">
          <input name="host" placeholder="host, e.g. api.github.com" required
            className="flex-1 min-w-48 rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal" />
          <input name="port" defaultValue="443" inputMode="numeric"
            className="w-20 rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal" />
          <select name="ttl" aria-label="Grant duration" defaultValue="2h" className="rounded-lg border border-border bg-surface px-3 py-2 text-sm">
            <option value="1h">1h</option><option value="2h">2h</option><option value="8h">8h</option><option value="24h">24h</option>
          </select>
        </div>
        <input name="reason" placeholder="why does this mission need it?" required
          className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal" />
        <button type="submit" disabled={pending}
          className="rounded-lg bg-signal px-4 py-2 text-sm font-semibold text-signal-fg disabled:opacity-50">
          {pending ? "Requesting…" : "Request access"}
        </button>
        {state.error && <p className="text-xs text-danger">{state.error}</p>}
        {state.ok && <p className="text-xs text-ok">{state.ok}</p>}
      </form>
    </section>
  );
}
