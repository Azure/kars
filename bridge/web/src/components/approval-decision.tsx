"use client";

// kars Bridge — approval decision controls (Approve / Deny).
//
// A client component so the decision is interactive (optional reason, pending
// state, inline error). The actual write is a server action — the browser
// never touches the cluster. The decider identity is supplied by the server
// (see config.operatorIdentity) and shown plainly with its honesty caveat.

import { useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import { decide } from "@/app/inbox/approval-actions";

export function ApprovalDecision({
  name,
  decider,
  authWired,
  resourceVersion,
  boundEnvelopeDigest,
  compact = false,
  requireReason = false,
  approveLabel = "Approve",
  denyLabel = "Deny",
  requireDenyReason = false,
  reasonPlaceholder,
}: {
  name: string;
  decider: string;
  authWired: boolean;
  resourceVersion: string;
  boundEnvelopeDigest: string | null;
  compact?: boolean;
  requireReason?: boolean;
  approveLabel?: string;
  denyLabel?: string;
  requireDenyReason?: boolean;
  reasonPlaceholder?: string;
}) {
  const router = useRouter();
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [pending, startTransition] = useTransition();

  function act(verdict: "approve" | "deny") {
    setError(null);
    if (verdict === "deny" && requireDenyReason && !reason.trim()) {
      setError("Describe the changes required before this work can run again.");
      return;
    }
    startTransition(async () => {
      const res = await decide(name, verdict, resourceVersion, boundEnvelopeDigest, reason);
      if (res.error) {
        setError(res.error);
      } else {
        router.refresh();
      }
    });
  }

  return (
    <div className="space-y-2">
      {(!compact || requireReason || requireDenyReason) && (
        <input
          type="text"
          value={reason}
          onChange={(e) => setReason(e.target.value)}
          placeholder={
            reasonPlaceholder
              ?? (requireReason
              ? "Answer required to continue this run"
              : "Reason (recorded in the receipt) — optional")
          }
          className="w-full rounded-lg border border-border bg-surface px-3 py-1.5 text-sm placeholder:text-foreground-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
        />
      )}
      {requireDenyReason && !reason.trim() && (
        <p className="text-xs text-foreground-muted">
          Written feedback is required to request changes.
        </p>
      )}
      <div className="flex items-center gap-2">
        <button
          type="button"
          disabled={pending || (requireReason && !reason.trim())}
          onClick={() => act("approve")}
          className="rounded-lg bg-signal px-3 py-1.5 text-sm font-medium text-signal-fg hover:opacity-90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal disabled:opacity-50"
        >
          {pending ? "…" : approveLabel}
        </button>
        <button
          type="button"
          disabled={pending || (requireDenyReason && !reason.trim())}
          onClick={() => act("deny")}
          className="rounded-lg border border-danger/40 bg-danger/10 px-3 py-1.5 text-sm font-medium text-danger hover:bg-danger/20 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-danger disabled:opacity-50"
        >
          {pending ? "…" : denyLabel}
        </button>
        <span className="text-xs text-foreground-muted">
          as <span className="font-mono">{decider}</span>
          {!authWired && (
            <span className="ml-1 text-warning" title="The Bridge has no authenticated session yet; decisions are attributed to the configured operator identity.">
              · shared operator identity
            </span>
          )}
        </span>
      </div>
      {error && (
        <p role="alert" className="text-xs text-danger">
          {error}
        </p>
      )}
    </div>
  );
}
