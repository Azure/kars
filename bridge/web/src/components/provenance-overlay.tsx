"use client";

// kars Bridge — Provenance Overlay. A standalone, slide-over lineage trail that
// makes a mission's full provenance chain legible in one place: trust envelope →
// signed in-toto predicate → claim matrix → tamper-evidence inclusion entry →
// signed checkpoint (tree head). It reads ONLY the real receipt; it never
// asserts cryptographic validity itself (that is `kars receipt verify`). This is
// the dedicated overlay the design note asks for — distinct from the inline
// receipt panel — surfacing the lineage as a navigable chain, not a form.

import { useState } from "react";
import type { ActivityEvent, Receipt } from "@/lib/types";
import { ProvenanceStory } from "@/components/provenance-story";

function Step({ ord, title, value, mono, hint, status }: { ord: number; title: string; value: string; mono?: boolean; hint?: string; status?: "ok" | "partial" | "omitted" }) {
  const dot = status === "ok" ? "bg-ok" : status === "partial" ? "bg-warning" : status === "omitted" ? "bg-foreground-muted/40" : "bg-signal";
  return (
    <li className="relative pl-7">
      <span className={`absolute left-1.5 top-1.5 h-2.5 w-2.5 rounded-full ${dot}`} />
      <p className="text-[11px] font-semibold uppercase tracking-wide text-foreground-muted">{ord}. {title}</p>
      <p className={`mt-0.5 break-all text-sm ${mono ? "font-mono text-xs" : ""}`}>{value}</p>
      {hint && <p className="mt-0.5 text-[11px] text-foreground-muted">{hint}</p>}
    </li>
  );
}

export function ProvenanceOverlay({ receipt, deliverableDid, activity, egress }: { receipt: Receipt; deliverableDid?: string | null; activity?: ActivityEvent[]; egress?: string[] }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button
        type="button"
        onClick={() => setOpen(true)}
        className="rounded-md border border-border px-3 py-1.5 text-xs font-medium hover:bg-surface focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
      >
        View provenance trail
      </button>
      {open && (
        <div className="fixed inset-0 z-50 flex justify-end bg-black/40" onClick={() => setOpen(false)}>
          <aside className="h-full w-full max-w-md overflow-y-auto border-l border-border bg-background p-6 shadow-xl" onClick={(e) => e.stopPropagation()}>
            <div className="flex items-center justify-between">
              <h2 className="text-sm font-semibold">Provenance trail</h2>
              <button type="button" onClick={() => setOpen(false)} className="text-foreground-muted hover:text-foreground" aria-label="Close">✕</button>
            </div>
            <p className="mt-1 text-xs text-foreground-muted">One legible chain from the granted envelope to the signed, witnessed log head. Render-only — verify with the command on the receipt.</p>
            {activity && activity.length > 0 && (
              <div className="mt-5 rounded-lg border border-border bg-surface p-4">
                <h3 className="text-xs font-semibold">What the agent actually did</h3>
                <div className="mt-2"><ProvenanceStory activity={activity} egress={egress} /></div>
              </div>
            )}
            <p className="mt-5 text-[11px] font-semibold uppercase tracking-wide text-foreground-muted">The cryptographic chain that proves the above</p>
            <ol className="mt-3 space-y-4 border-l border-border">
              <Step ord={1} title="Trust envelope" value={receipt.envelope_digest} mono hint="The exact authority granted — fingerprinted so it can't be altered after the fact" />
              <Step ord={2} title="Predicate" value={receipt.predicate_type} hint={`The signed statement of what ran (${receipt.scheme})`} />
              {receipt.claims.map((c, i) => (
                <Step key={c.class} ord={3 + i} title={`Claim · ${c.class}`} value={c.status} status={c.status === "PASS" ? "ok" : c.status === "PARTIAL" ? "partial" : "omitted"} />
              ))}
              {receipt.inclusion_seq != null && (
                <Step ord={3 + receipt.claims.length} title="Inclusion entry" value={`seq ${receipt.inclusion_seq} · ${receipt.inclusion_entry_hash ?? ""}`} mono hint="Logged so removing or altering it would break the chain" />
              )}
              {receipt.checkpoint && (
                <Step ord={4 + receipt.claims.length} title="Signed checkpoint" value={`tree=${receipt.checkpoint.tree_size} root=${receipt.checkpoint.root_hash}`} mono hint={`A second key co-signs the whole log so it can't be forked (key ${receipt.checkpoint.key_id})`} status="ok" />
              )}
              {deliverableDid && (
                <Step ord={5 + receipt.claims.length} title="Deliverable identity" value={deliverableDid} mono hint="Content-addressed output" status="ok" />
              )}
            </ol>
          </aside>
        </div>
      )}
    </>
  );
}
