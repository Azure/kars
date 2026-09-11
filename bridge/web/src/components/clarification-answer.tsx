"use client";

// kars Bridge — clarification answer control. A team run asked the human a
// question (a `clarification` KarsApproval raised by the principal). The human
// types an answer here; it is recorded on the same decision path (verdict
// "approve", the answer as the reason), and the controller delivers it into the
// team's commons so the next run reads it. Answering IS the approval — there is
// no separate "deny" for a question, though the human can dismiss it.

import { useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import { decide } from "@/app/inbox/approval-actions";

export function ClarificationAnswer({
  name,
  decider,
  authWired,
  resourceVersion,
  boundEnvelopeDigest,
}: {
  name: string;
  decider: string;
  authWired: boolean;
  resourceVersion: string;
  boundEnvelopeDigest: string | null;
}) {
  const router = useRouter();
  const [answer, setAnswer] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [pending, startTransition] = useTransition();

  function submit(verdict: "approve" | "deny") {
    setError(null);
    if (verdict === "approve" && answer.trim().length === 0) {
      setError("Type an answer so the team can proceed.");
      return;
    }
    startTransition(async () => {
      const res = await decide(
        name,
        verdict,
        resourceVersion,
        boundEnvelopeDigest,
        answer.trim() || undefined,
      );
      if (res.error) {
        setError(res.error);
      } else {
        router.refresh();
      }
    });
  }

  return (
    <div className="space-y-2">
      <textarea
        value={answer}
        onChange={(e) => setAnswer(e.target.value)}
        rows={2}
        placeholder="Your answer — delivered to the active run…"
        className="w-full resize-y rounded-lg border border-border bg-surface px-3 py-1.5 text-sm placeholder:text-foreground-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
      />
      <div className="flex items-center gap-2">
        <button
          type="button"
          disabled={pending}
          onClick={() => submit("approve")}
          className="rounded-lg bg-signal px-3 py-1.5 text-sm font-medium text-signal-fg hover:opacity-90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal disabled:opacity-50"
        >
          {pending ? "…" : "Send answer"}
        </button>
        <button
          type="button"
          disabled={pending}
          onClick={() => submit("deny")}
          className="rounded-lg border border-border px-3 py-1.5 text-sm font-medium text-foreground-muted hover:bg-surface-muted disabled:opacity-50"
          title="Dismiss without answering — the team is told no guidance is coming."
        >
          {pending ? "…" : "Dismiss"}
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
      {error && <p role="alert" className="text-xs text-danger">{error}</p>}
    </div>
  );
}
