// kars Bridge — the Journey rail. ONE lifecycle spine, rendered identically
// across the product so every surface tells the same story: a unit of work
// (mission or team) always moves through Describe → Compose → Review → Launch →
// Build → Run → Deliver. Showing the same seven beats everywhere is what makes
// the product legible: wherever you are, you can read where you are and what
// comes next. Purely presentational — `current` is derived by each surface from
// real state (compose step, task phase, execution phase).

export const JOURNEY_BEATS = [
  { id: "describe", label: "Describe", hint: "State the intent" },
  { id: "compose", label: "Compose", hint: "Envelope assembled" },
  { id: "review", label: "Review", hint: "Edit & verify" },
  { id: "launch", label: "Launch", hint: "You approve the start" },
  { id: "build", label: "Build", hint: "Provision & verify access" },
  { id: "run", label: "Run", hint: "Live work & steering" },
  { id: "deliver", label: "Deliver", hint: "Artifacts & receipt" },
] as const;

export type JourneyBeat = (typeof JOURNEY_BEATS)[number]["id"];

/** Map a mission's real state to the current journey beat. */
export function missionBeat(opts: {
  launched: boolean;
  executionPhase: string | null;
  hasResult: boolean;
  blocked?: boolean;
}): JourneyBeat {
  if (opts.hasResult) return "deliver";
  if (opts.executionPhase === "Running") return "run";
  if (opts.executionPhase === "Launching" || opts.executionPhase === "Pending") return "build";
  if (opts.launched) return "run";
  return "launch";
}

/** Map a standing team's state to the current beat (teams live mostly in Run). */
export function teamBeat(opts: { paused: boolean; everRan: boolean }): JourneyBeat {
  // A team paused AFTER it has run is still a Run-phase unit that's simply
  // hibernating — surface it on "run" (the caller passes `blocked` to render
  // the distinct paused treatment) rather than rewinding it to "review", which
  // wrongly implied a team with 20+ delivered runs was still awaiting review.
  // Only a team paused BEFORE it ever launched legitimately sits pre-run.
  if (opts.paused) return opts.everRan ? "run" : "review";
  return opts.everRan ? "run" : "build";
}

export function JourneyRail({
  current,
  blocked = false,
  paused = false,
  compact = false,
}: {
  current: JourneyBeat;
  blocked?: boolean;
  paused?: boolean;
  compact?: boolean;
}) {
  const idx = JOURNEY_BEATS.findIndex((b) => b.id === current);
  return (
    <nav aria-label="Mission journey" className="kb-rise">
      <ol className="flex items-stretch gap-1 overflow-x-auto rounded-xl border border-border bg-surface/70 p-1.5">
        {JOURNEY_BEATS.map((b, i) => {
          const state = i < idx ? "done" : i === idx ? "current" : "upcoming";
          const isBlocked = i === idx && blocked;
          // Paused is a HEALTHY resting state (a hibernating standing team), not
          // an error — render it as a muted "on hold" beat, distinct from the
          // red `blocked` attention state.
          const isPaused = i === idx && paused && !isBlocked;
          return (
            <li key={b.id} className="flex min-w-0 flex-1 items-center gap-1">
              <div
                className={[
                  "flex min-w-0 flex-1 items-center gap-2 rounded-lg px-2.5 py-1.5 transition",
                  state === "current" && !isBlocked && !isPaused
                    ? "bg-signal/10"
                    : isBlocked
                      ? "bg-danger/10"
                      : isPaused
                        ? "bg-surface-muted"
                        : "",
                ].join(" ")}
              >
                <span
                  className={[
                    "grid h-5 w-5 shrink-0 place-items-center rounded-full text-[10px] font-semibold",
                    state === "done"
                      ? "bg-signal text-signal-fg"
                      : isBlocked
                        ? "bg-danger text-white"
                        : isPaused
                          ? "bg-surface-muted text-foreground-muted ring-1 ring-border"
                          : state === "current"
                            ? "bg-signal/20 text-signal ring-2 ring-signal/40"
                            : "bg-surface-muted text-foreground-muted",
                  ].join(" ")}
                  aria-hidden
                >
                  {state === "done" ? "✓" : isBlocked ? "!" : isPaused ? "‖" : i + 1}
                </span>
                <span className="min-w-0">
                  <span
                    className={[
                      "block whitespace-nowrap text-xs font-medium leading-tight",
                      state === "upcoming" ? "text-foreground-muted" : "text-foreground",
                    ].join(" ")}
                  >
                    {b.label}
                  </span>
                  {!compact && (
                    <span className="hidden truncate text-[10px] leading-tight text-foreground-muted lg:block">
                      {state === "current" && isBlocked
                        ? "Needs your attention"
                        : isPaused
                          ? "Paused — resumes on Run now"
                          : b.hint}
                    </span>
                  )}
                </span>
              </div>
              {i < JOURNEY_BEATS.length - 1 && (
                <span
                  aria-hidden
                  className={`h-px w-3 shrink-0 ${i < idx ? "bg-signal" : "bg-border"}`}
                />
              )}
            </li>
          );
        })}
      </ol>
    </nav>
  );
}
