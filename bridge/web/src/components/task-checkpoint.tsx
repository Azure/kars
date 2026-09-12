import type { TaskCheckpoint } from "@/lib/types";

const TONE: Record<TaskCheckpoint["status"], string> = {
  pending: "border-border bg-surface-muted/30",
  in_progress: "border-sky-500/40 bg-sky-500/[0.06]",
  completed: "border-emerald-500/40 bg-emerald-500/[0.06]",
  blocked: "border-warning/50 bg-warning/[0.08]",
};

export function TaskCheckpointPanel({ checkpoint }: { checkpoint: TaskCheckpoint }) {
  return (
    <section className={`rounded-xl border p-5 ${TONE[checkpoint.status]}`}>
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <p className="text-[11px] font-semibold uppercase tracking-wide text-foreground-muted">
            Durable milestone checkpoint
          </p>
          <h2 className="mt-1 text-sm font-semibold">
            {checkpoint.milestone_id.replaceAll("-", " ")}
          </h2>
        </div>
        <span className="rounded-full border border-current/20 px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wide">
          {checkpoint.status.replaceAll("_", " ")}
        </span>
      </div>
      <p className="mt-2 text-sm text-foreground-muted">{checkpoint.summary}</p>
      {(checkpoint.acceptance_criteria?.length ?? 0) > 0 && (
        <div className="mt-3">
          <p className="text-xs font-semibold">Acceptance criteria</p>
          <ul className="mt-1 space-y-1 text-xs text-foreground-muted">
            {checkpoint.acceptance_criteria!.map((criterion) => (
              <li key={criterion}>• {criterion}</li>
            ))}
          </ul>
        </div>
      )}
      {(checkpoint.artifacts?.length ?? 0) > 0 && (
        <p className="mt-3 text-xs text-foreground-muted">
          <span className="font-semibold text-foreground">Owned artifacts:</span>{" "}
          {checkpoint.artifacts!.join(", ")}
        </p>
      )}
      {(checkpoint.next_steps?.length ?? 0) > 0 && (
        <p className="mt-2 text-xs text-foreground-muted">
          <span className="font-semibold text-foreground">Next:</span>{" "}
          {checkpoint.next_steps!.join(" · ")}
        </p>
      )}
    </section>
  );
}
