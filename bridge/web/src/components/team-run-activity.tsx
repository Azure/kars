import type { TeamRunEvidence } from "@/lib/team-run-evidence";

function label(value: string): string {
  return value.replace(/_/g, " ").replace(/^\w/, (c) => c.toUpperCase());
}

export function TeamRunActivity({ evidence }: { evidence: TeamRunEvidence }) {
  const events = [
    ...evidence.collaboration.map((event) => ({
      at: event.at,
      kind: "collaboration" as const,
      title: event.event === "mcp_tool_call" ? "MCP tool call" : label(event.event),
      actor: event.member ?? event.agent,
      detail: event.preview,
      outcome: event.outcome,
      href: null,
      meta: event.source === "ledger"
       ? "core-ledger"
       : event.source === "router"
         ? "router-authoritative"
         : event.source === "agent-reported"
           ? "agent-reported"
           : "recovered",
    })),
    ...evidence.research.map((event) => ({
      at: event.at,
      kind: "research" as const,
      title: event.outcome === "success" ? "External source reached" : "External source blocked or failed",
      actor: event.agent,
      detail: event.host,
      outcome: event.outcome,
      href: event.url,
      meta: event.source === "router"
        ? event.status ? `router · HTTP ${event.status}` : "router · governed egress"
        : "agent-reported",
    })),
  ].sort((a, b) => (a.at ?? "").localeCompare(b.at ?? ""));
  const delivered = evidence.roles.filter((role) => role.state === "delivered").length;
  const working = evidence.roles.filter((role) => role.state === "working").length;

  return (
    <section className="rounded-xl border border-border bg-surface p-5">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">Live team storyline</h2>
          <p className="mt-1 text-xs text-foreground-muted">
            Purposeful delegation and handbacks toward the run outcome. Infrastructure events are
            retained below as evidence, not presented as the work itself.
          </p>
        </div>
        <span className="rounded-full border border-border bg-surface-muted px-2.5 py-1 text-xs font-medium text-foreground-muted">
          {working > 0
            ? `${working} working · ${delivered} delivered`
            : `${delivered}/${evidence.roles.length} roles delivered`}
        </span>
      </div>
      <ol className="mt-4 grid gap-3 md:grid-cols-3">
        {evidence.roles.map((role) => {
          const normalize = (value: string) =>
            value.toLowerCase().replace(/[^a-z0-9]+/g, "");
          const handback =
            role.handbacks.at(-1) ??
            evidence.collaboration
              .filter(
                (event) =>
                  event.preview &&
                  event.member &&
                  normalize(event.member) === normalize(role.role.name),
              )
              .at(-1);
          const tone =
            role.state === "delivered"
              ? "border-emerald-500/30 bg-emerald-500/[0.06]"
              : role.state === "working"
                ? "border-sky-500/30 bg-sky-500/[0.06]"
                : role.state === "failed" || role.state === "missing"
                  ? "border-rose-500/30 bg-rose-500/[0.06]"
                  : "border-border bg-surface-muted/25";
          return (
            <li key={role.role.name} className={`rounded-xl border p-4 ${tone}`}>
              <div className="flex items-center justify-between gap-2">
                <span className="font-mono text-xs font-semibold">{role.role.name}</span>
                <span className="rounded-full border border-current/20 px-2 py-0.5 text-[10px] font-medium capitalize">
                  {role.state}
                </span>
              </div>
              <p className="mt-2 line-clamp-4 text-xs leading-relaxed text-foreground-muted">
                {handback?.preview ??
                  (role.state === "working"
                    ? "Working on the assigned packet."
                    : role.state === "skipped"
                      ? "Not needed for this run."
                      : "No structured handback is available.")}
              </p>
              <p className="mt-3 text-[10px] text-foreground-muted">
                {role.artifacts.length} evidence file{role.artifacts.length === 1 ? "" : "s"}
                {handback?.at ? ` · ${new Date(handback.at).toLocaleTimeString()}` : ""}
              </p>
            </li>
          );
        })}
      </ol>
      {evidence.research.length > 0 && (
        <p className="mt-4 rounded-lg border border-border bg-surface-muted/25 px-3 py-2 text-xs text-foreground-muted">
          Governed external evidence: {evidence.research.length} request
          {evidence.research.length === 1 ? "" : "s"} across{" "}
          {new Set(evidence.research.map((event) => event.host).filter(Boolean)).size} host
          {new Set(evidence.research.map((event) => event.host).filter(Boolean)).size === 1
            ? ""
            : "s"}
          .
        </p>
      )}
      <details className="mt-4 rounded-lg border border-border bg-surface-muted/20">
        <summary className="cursor-pointer px-4 py-3 text-xs font-medium">
          Evidence timeline
          <span className="ml-2 font-normal text-foreground-muted">
            {events.length} ledger, agent, and router event{events.length === 1 ? "" : "s"}
          </span>
        </summary>
        {events.length === 0 ? (
          <p className="border-t border-border px-4 py-3 text-xs text-foreground-muted">
            This run predates structured evidence or has not emitted an event yet.
          </p>
        ) : (
          <ol className="space-y-2 border-t border-border p-3">
            {events.map((event, index) => (
              <li
                key={`${event.kind}-${event.at ?? "unknown"}-${index}`}
                className="min-w-0 overflow-hidden rounded-lg border border-border bg-surface px-3 py-2"
              >
                <div className="flex min-w-0 flex-wrap items-center gap-2 text-xs">
                  <span
                    className={`h-1.5 w-1.5 rounded-full ${event.kind === "research" ? "bg-sky-500" : "bg-signal"}`}
                    aria-hidden
                  />
                  <span className="font-medium">{event.title}</span>
                  {event.actor && (
                    <span className="max-w-64 truncate font-mono text-[10px] text-foreground-muted">
                      {event.actor}
                    </span>
                  )}
                  <span className="ml-auto text-[9px] uppercase tracking-wide text-foreground-muted">
                    {event.meta}
                  </span>
                </div>
                {event.detail && (
                  <p className="mt-1 line-clamp-2 break-words text-[11px] text-foreground-muted">
                    {event.detail}
                  </p>
                )}
              </li>
            ))}
          </ol>
        )}
      </details>
    </section>
  );
}
