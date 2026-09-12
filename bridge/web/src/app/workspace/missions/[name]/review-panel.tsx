"use client";

// kars Bridge Workspace — the artifact review loop (§16). A reviewer accepts a
// deliverable or requests changes; request-changes re-drives the producing task
// on the delta and a new revision lands. Typed by artifact kind, with the full
// review lineage and a link into the run's provenance (the execution trace).

import { useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import { submitReview } from "./review-actions";
import { setLaunch } from "@/app/tasks/[name]/launch-actions";
import type { ReviewState } from "@/lib/types";

const KIND_LABEL: Record<string, string> = {
  code: "Code — review as a change",
  docs: "Document — review the prose",
  data: "Data — review the values",
  output: "Output — review the result",
};

function statusChip(status: string, pending: boolean) {
  if (pending)
    return { label: "Revising…", cls: "bg-sky-500/10 text-sky-600 border-sky-500/30" };
  switch (status) {
    case "approved":
      return { label: "Approved", cls: "bg-emerald-500/10 text-emerald-600 border-emerald-500/30" };
    case "changes_requested":
      return { label: "Changes requested", cls: "bg-amber-500/10 text-amber-600 border-amber-500/30" };
    default:
      return { label: "Awaiting review", cls: "bg-surface-muted text-foreground-muted border-border" };
  }
}

export function ReviewPanel({
  task,
  assignmentNonce,
  kind,
  initial,
  sandboxLive = false,
}: {
  task: string;
  assignmentNonce: string;
  kind: string;
  initial: ReviewState | null;
  /** True when the producing agent's sandbox is still Running — so approving can
   *  offer to terminate it (cleanup-on-good). False once it's already torn down. */
  sandboxLive?: boolean;
}) {
  const router = useRouter();
  const [pending, startTransition] = useTransition();
  const [comment, setComment] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [requesting, setRequesting] = useState(false);
  // After Approve, we ask whether to terminate the agent (cleanup-on-good). The
  // operator can dismiss to leave it running (e.g. to iterate further).
  const [keepRunning, setKeepRunning] = useState(false);

  const status = initial?.status ?? "none";
  const redrivePending = initial?.redrive_pending ?? false;
  const revision = initial?.revision ?? 0;
  const history = initial?.history ?? [];
  const chip = statusChip(status, redrivePending);

  function act(decision: "approve" | "request_changes") {
    setError(null);
    if (decision === "request_changes" && comment.trim().length === 0) {
      setError("Describe what needs to change so the agent can revise.");
      return;
    }
    startTransition(async () => {
      const res = await submitReview(
        task,
        assignmentNonce,
        decision,
        comment.trim() || undefined,
      );
      if (res.error) {
        setError(res.error);
        return;
      }
      setComment("");
      setRequesting(false);
      router.refresh();
    });
  }

  function terminateAgent() {
    setError(null);
    startTransition(async () => {
      const res = await setLaunch(task, false);
      if (res.error) {
        setError(res.error);
        return;
      }
      router.refresh();
    });
  }

  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">Review</h2>
          <p className="mt-0.5 text-xs text-foreground-muted">
            {KIND_LABEL[kind] ?? KIND_LABEL.output} · accept the deliverable or request changes —
            requesting changes re-runs the agent on your feedback.
          </p>
        </div>
        <span className={`shrink-0 rounded-full border px-2.5 py-1 text-xs font-medium ${chip.cls}`}>
          {chip.label}
          {revision > 0 && <span className="ml-1 opacity-70">· rev {revision}</span>}
        </span>
      </div>

      {redrivePending ? (
        <p className="mt-4 rounded-lg border border-sky-500/30 bg-sky-500/5 px-4 py-3 text-sm text-foreground-muted">
          <span className="font-medium text-sky-700">Changes requested — received.</span>{" "}
          The agent is producing a new revision from your feedback. It will land here when ready
          (watch it in the Activity tab).
        </p>
      ) : (
        <div className="mt-4 space-y-3">
          {status === "approved" && !requesting ? (
            <div className="space-y-3">
              <p className="rounded-lg border border-emerald-500/30 bg-emerald-500/5 px-4 py-3 text-sm text-emerald-700">
                ✓ You approved this deliverable{revision > 0 ? ` (rev ${revision})` : ""}.{" "}
                <button
                  type="button"
                  onClick={() => setRequesting(true)}
                  className="underline hover:no-underline"
                >
                  Request changes to iterate
                </button>
                .
              </p>
              {/* Cleanup-on-good: once approved, offer to terminate the agent and
                  free its sandbox. The deliverable + signed receipt are kept — this
                  only tears down the running agent. Shown only while the sandbox is
                  still up, and dismissable if the operator wants to keep iterating. */}
              {sandboxLive && !keepRunning && (
                <div className="rounded-lg border border-border bg-surface-muted/40 px-4 py-3">
                  <p className="text-sm font-medium">Happy with this? You can free the agent now.</p>
                  <p className="mt-0.5 text-xs text-foreground-muted">
                    Terminating tears down the agent&rsquo;s sandbox and frees its resources. Your
                    deliverable and its signed receipt are kept — nothing is lost.
                  </p>
                  <div className="mt-3 flex flex-wrap gap-2">
                    <button
                      type="button"
                      disabled={pending}
                      onClick={terminateAgent}
                      className="rounded-lg bg-emerald-600 px-4 py-2 text-sm font-semibold text-white transition hover:opacity-90 disabled:opacity-50"
                    >
                      {pending ? "Freeing…" : "Terminate agent & free sandbox"}
                    </button>
                    <button
                      type="button"
                      disabled={pending}
                      onClick={() => setKeepRunning(true)}
                      className="rounded-lg border border-border bg-surface px-4 py-2 text-sm font-medium transition hover:bg-surface-muted disabled:opacity-50"
                    >
                      Keep it running
                    </button>
                  </div>
                </div>
              )}
              {sandboxLive && keepRunning && (
                <p className="text-xs text-foreground-muted">
                  The agent stays running — free it any time from the Execution panel
                  (&ldquo;Stop sandbox&rdquo;).
                </p>
              )}
            </div>
          ) : !requesting ? (
            <div className="flex flex-wrap gap-2">
              <button
                type="button"
                disabled={pending}
                onClick={() => act("approve")}
                className="rounded-lg bg-emerald-600 px-4 py-2 text-sm font-semibold text-white transition hover:opacity-90 disabled:opacity-50"
              >
                Approve deliverable
              </button>
              <button
                type="button"
                disabled={pending}
                onClick={() => setRequesting(true)}
                className="rounded-lg border border-border bg-surface px-4 py-2 text-sm font-medium transition hover:bg-surface-muted disabled:opacity-50"
              >
                Request changes
              </button>
            </div>
          ) : (
            <div className="space-y-2">
              <textarea
                value={comment}
                onChange={(e) => setComment(e.target.value)}
                rows={3}
                placeholder="What should change? Be specific — the agent revises against this."
                className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
              />
              <div className="flex flex-wrap gap-2">
                <button
                  type="button"
                  disabled={pending}
                  onClick={() => act("request_changes")}
                  className="rounded-lg bg-signal px-4 py-2 text-sm font-semibold text-signal-fg transition hover:opacity-90 disabled:opacity-50"
                >
                  {pending ? "Sending…" : "Send & re-run"}
                </button>
                <button
                  type="button"
                  disabled={pending}
                  onClick={() => {
                    setRequesting(false);
                    setComment("");
                    setError(null);
                  }}
                  className="rounded-lg border border-border bg-surface px-4 py-2 text-sm font-medium transition hover:bg-surface-muted disabled:opacity-50"
                >
                  Cancel
                </button>
              </div>
            </div>
          )}
          {error && <p className="text-xs text-rose-600">{error}</p>}
        </div>
      )}

      {history.length > 0 && (
        <div className="mt-5 border-t border-border pt-4">
          <p className="text-xs font-medium text-foreground-muted">Review history</p>
          <ul className="mt-2 space-y-2">
            {history.map((h, i) => (
              <li key={i} className="text-xs">
                <span
                  className={
                    h.decision === "approve" ? "font-medium text-emerald-600" : "font-medium text-amber-600"
                  }
                >
                  {h.decision === "approve" ? "Approved" : "Requested changes"}
                </span>{" "}
                <span className="text-foreground-muted">
                  · rev {h.revision} · {new Date(h.decided_at).toLocaleString()} · {h.reviewer}
                  {h.attested === false && (
                    <span
                      className="ml-1 rounded bg-surface-muted px-1 py-0.5 text-[10px] text-foreground-muted"
                      title="Self-reported name — not verified against an authenticated identity in this deployment."
                    >
                      self-reported
                    </span>
                  )}
                </span>
                {h.comment && <p className="mt-0.5 text-foreground-muted">“{h.comment}”</p>}
              </li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}
