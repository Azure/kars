// kars Bridge Workspace — mission status projection.
//
// Projects operator governance vocabulary (Ready/Degraded/Pending + execution
// phase) into plain user language. The Workspace never shows "Degraded" or
// "Pending" — it shows what the user actually cares about.

export type MissionStatus =
  | "drafting"
  | "deploying"
  | "running"
  | "needs_you"
  | "done"
  | "failed"
  | "blocked";

/** Map a governance phase (+ optional execution phase) to user language.
 *  ONE projection, used by every surface (page badge, execution panel, fleet
 *  card) so the mission never shows two different states at once.
 *  `delivered` is the authoritative "work is done" signal (a REAL ok result);
 *  `failed` is a captured error result (run completed but did not succeed);
 *  `needsYou` surfaces a pending human decision; `launched` distinguishes a
 *  deploying mission from an un-launched draft. */
export function missionStatus(
  phase: string | null,
  executionPhase?: string | null,
  opts?: { delivered?: boolean; needsYou?: boolean; launched?: boolean; failed?: boolean },
): MissionStatus {
  // A captured error result is terminal — it outranks a stale "Running" phase
  // so the mission never reads "Running" while its run has already failed.
  if (opts?.failed) return "failed";
  if (phase === "Degraded") return "blocked";
  if (executionPhase === "Degraded") return "blocked";
  if (opts?.needsYou) return "needs_you";
  // A captured deliverable is the terminal, authoritative outcome — it must
  // outrank "Running" too, exactly like `failed` above. The sandbox pod can
  // stay alive after its mesh task-delivery already produced a result (e.g.
  // an idle-daemon Hermes/OpenClaw agent waiting for the next inbound), so
  // "pod still running" must never mask an already-delivered mission.
  if (opts?.delivered) return "done";
  if (executionPhase === "Running") return "running";
  // Launched but not yet Running and nothing delivered → the sandbox is
  // materializing. This is "Deploying", NOT "Ready to launch" — the badge, the
  // execution panel, and the deploy timeline all agree on this single state.
  if (opts?.launched) return "deploying";
  // Governed but idle (the §20 review-then-launch default) reads as "drafting"
  // to the user — it's planned, not yet running.
  if (phase === "Ready") return "drafting";
  return "drafting";
}

const META: Record<MissionStatus, { label: string; cls: string }> = {
  drafting: { label: "Ready to launch", cls: "border-border bg-surface-muted text-foreground-muted" },
  deploying: { label: "Deploying", cls: "border-signal/40 bg-signal/10 text-signal" },
  running: { label: "Running", cls: "border-signal/40 bg-signal/10 text-signal" },
  needs_you: { label: "Needs you", cls: "border-warning/40 bg-warning/10 text-warning" },
  done: { label: "Delivered", cls: "border-ok/40 bg-ok/10 text-ok" },
  failed: { label: "Run failed", cls: "border-danger/40 bg-danger/10 text-danger" },
  blocked: { label: "Blocked", cls: "border-danger/40 bg-danger/10 text-danger" },
};

export function MissionStatusBadge({ status }: { status: MissionStatus }) {
  const m = META[status];
  return (
    <span
      className={`inline-flex shrink-0 items-center gap-1.5 rounded-full border px-2.5 py-0.5 text-xs font-medium ${m.cls}`}
    >
      <span className="h-1.5 w-1.5 rounded-full bg-current" aria-hidden />
      {m.label}
    </span>
  );
}
