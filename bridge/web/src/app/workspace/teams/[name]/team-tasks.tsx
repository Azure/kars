"use client";

// kars Bridge — team task backlog. A standing team is a persistent org you
// assign discrete tasks to (a, b, c, d). Each run picks up the oldest pending
// task, works it, and marks it done — so a "finance" or "marketing" team keeps a
// visible, progressing worklist beyond its always-on charter.

import { useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import Link from "next/link";
import type { TeamTask } from "@/lib/types";
import { addTeamTask, deleteTeamTask, reviewTeamTask } from "./task-actions";

const STATUS_META: Record<string, { label: string; cls: string; glyph: string }> = {
  pending: {
    label: "Queued",
    cls: "border-border bg-surface-muted text-foreground-muted",
    glyph: "○",
  },
  active: {
    label: "Running",
    cls: "border-sky-500/40 bg-sky-500/10 text-sky-600",
    glyph: "◐",
  },
  awaiting_review: {
    label: "Awaiting review",
    cls: "border-amber-500/40 bg-amber-500/10 text-amber-600",
    glyph: "◆",
  },
  done: {
    label: "Done",
    cls: "border-emerald-500/40 bg-emerald-500/10 text-emerald-600",
    glyph: "✓",
  },
};

export function TeamTasks({
  team,
  tasks,
  paused,
}: {
  team: string;
  tasks: TeamTask[];
  paused: boolean;
}) {
  const router = useRouter();
  const [pending, startTransition] = useTransition();
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [reviewFeedback, setReviewFeedback] = useState<Record<string, string>>({});
  const [showCompleted, setShowCompleted] = useState(false);

  const pendingCount = tasks.filter((t) => t.status === "pending").length;
  const activeCount = tasks.filter((t) => t.status === "active").length;
  const completedCount = tasks.filter((t) => t.status === "done").length;
  const completedIds = new Set(tasks.filter((task) => task.status === "done").map((task) => task.id));
  const visibleTasks = [
    ...tasks.filter((task) => task.status === "active"),
    ...tasks.filter((task) => task.status === "awaiting_review"),
    ...tasks.filter((task) => task.status === "pending"),
    ...(showCompleted ? tasks.filter((task) => task.status === "done") : []),
  ];

  function submit() {
    if (!title.trim()) return;
    setError(null);
    startTransition(async () => {
      const res = await addTeamTask(team, title.trim(), description.trim());
      if (res.error) {
        setError(res.error);
        return;
      }
      setTitle("");
      setDescription("");
      router.refresh();
    });
  }

  function remove(id: string) {
    setError(null);
    startTransition(async () => {
      const res = await deleteTeamTask(team, id);
      if (res?.error) {
        setError(res.error);
        return;
      }
      router.refresh();
    });
  }

  function review(id: string, decision: "approve" | "request_changes") {
    setError(null);
    startTransition(async () => {
      const result = await reviewTeamTask(team, id, decision, reviewFeedback[id]);
      if (result.error) {
        setError(result.error);
        return;
      }
      router.refresh();
    });
  }

  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">Task backlog</h2>
          <p className="mt-0.5 text-xs text-foreground-muted">
            Discrete tasks this team works through, one per run — beyond its always-on charter. The
            team picks up the oldest queued task on its next run and marks it done when delivered.
          </p>
        </div>
        {tasks.length > 0 && (
          <span className="shrink-0 rounded-full bg-surface-muted px-2.5 py-1 text-xs font-medium text-foreground-muted">
            {activeCount > 0 ? `${activeCount} running · ` : ""}
            {pendingCount} queued
          </span>
        )}
      </div>

      {/* Add task */}
      <div className="mt-4 rounded-lg border border-border bg-surface-muted/30 p-3">
        <input
          type="text"
          value={title}
          onChange={(e) => setTitle(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              submit();
            }
          }}
          placeholder="Task title — e.g. Draft Q3 board deck outline"
          className="w-full rounded-md border border-border bg-surface px-3 py-2 text-sm outline-none focus:border-signal"
        />
        <textarea
          value={description}
          onChange={(e) => setDescription(e.target.value)}
          placeholder="Optional details, constraints, links…"
          rows={2}
          className="mt-2 w-full resize-y rounded-md border border-border bg-surface px-3 py-2 text-xs outline-none focus:border-signal"
        />
        <div className="mt-2 flex items-center justify-between gap-3">
          <p className="text-[11px] text-foreground-muted">
            {paused
              ? "Team is paused — queued tasks run once you resume it."
              : "Runs on the next cadence tick, or immediately via Run now."}
          </p>
          <button
            type="button"
            disabled={pending || !title.trim()}
            onClick={submit}
            title={!title.trim() ? "Enter a task title first" : undefined}
            className="rounded-lg bg-signal px-3 py-1.5 text-xs font-semibold text-signal-fg transition hover:opacity-90 disabled:opacity-50"
          >
            {pending ? "Adding…" : "+ Add task"}
          </button>
          {!title.trim() && !pending && (
            <span className="text-[11px] text-foreground-muted">Enter a title to add</span>
          )}
        </div>
        {error && <p className="mt-1.5 text-xs text-rose-600">{error}</p>}
      </div>

      {/* Backlog list */}
      {tasks.length === 0 ? (
        <p className="mt-4 text-xs text-foreground-muted">
          No tasks yet. Add one above to give this team discrete work to progress through.
        </p>
      ) : (
        <>
        <ul className="mt-4 space-y-2">
          {visibleTasks.map((t) => {
            const meta = STATUS_META[t.status] ?? STATUS_META.pending;
            const blockedBy = t.depends_on.filter((dependency) => !completedIds.has(dependency));
            return (
              <li
                key={t.id}
                className="flex items-start justify-between gap-3 rounded-lg border border-border px-3 py-2.5"
              >
                <div className="min-w-0">
                  <div className="flex items-center gap-2">
                    <span
                      className={`inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[10px] font-medium ${meta.cls}`}
                    >
                      <span aria-hidden>{meta.glyph}</span> {meta.label}
                    </span>
                    <p className="truncate text-sm font-medium">{t.title}</p>
                  </div>
                  {t.description && (
                    <p className="mt-0.5 line-clamp-2 text-xs text-foreground-muted">
                      {t.description}
                    </p>
                  )}
                  {blockedBy.length > 0 && (
                    <p className="mt-1 text-[11px] text-warning">
                      Waiting for: {blockedBy.join(", ")}
                    </p>
                  )}
                  {t.acceptance_criteria.length > 0 && (
                    <p className="mt-1 text-[11px] text-foreground-muted">
                      Acceptance: {t.acceptance_criteria.join(" · ")}
                    </p>
                  )}
                  {t.run && (
                    <Link
                      href={`/workspace/teams/${encodeURIComponent(team)}/runs/${encodeURIComponent(t.run)}`}
                      className="mt-1 inline-block text-[11px] text-signal hover:underline"
                    >
                      {t.status === "done" ? "View result →" : "View run →"}
                    </Link>
                  )}
                  {t.status === "awaiting_review" && (
                    <div className="mt-2 space-y-2">
                      <textarea
                        value={reviewFeedback[t.id] ?? ""}
                        onChange={(event) =>
                          setReviewFeedback((current) => ({
                            ...current,
                            [t.id]: event.target.value,
                          }))
                        }
                        rows={2}
                        placeholder="Feedback required when requesting changes"
                        className="w-full rounded-md border border-border bg-surface px-2.5 py-1.5 text-xs"
                      />
                      <div className="flex flex-wrap gap-2">
                        <button
                          type="button"
                          disabled={pending}
                          onClick={() => review(t.id, "approve")}
                          className="rounded-md bg-signal px-2.5 py-1 text-[11px] font-semibold text-signal-fg disabled:opacity-50"
                        >
                          Approve milestone
                        </button>
                        <button
                          type="button"
                          disabled={pending || !(reviewFeedback[t.id] ?? "").trim()}
                          onClick={() => review(t.id, "request_changes")}
                          className="rounded-md border border-warning/50 px-2.5 py-1 text-[11px] font-semibold text-warning disabled:opacity-50"
                        >
                          Request changes
                        </button>
                      </div>
                    </div>
                  )}
                </div>
                <button
                  type="button"
                  disabled={pending}
                  onClick={() => remove(t.id)}
                  title="Remove from backlog"
                  className="shrink-0 rounded-md border border-border px-2 py-1 text-[11px] text-foreground-muted transition hover:bg-surface-muted hover:text-rose-600 disabled:opacity-50"
                >
                  Remove
                </button>
              </li>
            );
          })}
        </ul>
        {completedCount > 0 && (
          <button
            type="button"
            onClick={() => setShowCompleted((value) => !value)}
            className="mt-3 text-[11px] font-medium text-signal hover:underline"
          >
            {showCompleted
              ? "Hide completed work"
              : `Show ${completedCount} completed item${completedCount === 1 ? "" : "s"}`}
          </button>
        )}
        </>
      )}
    </section>
  );
}
