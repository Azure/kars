// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { revisionArtifactUrl } from "@/lib/mission-run-evidence";
import { ROUTER_OBSERVATIONS_ARTIFACT, routerObservationKey, routerProducerKey, toolResultSummary, type BoundRouterActivity, type RouterActivityEvent } from "@/lib/router-activity";

const measured = (value: number | null | undefined) => value == null ? "Unavailable" : String(value);

function Observation({ event }: { event: RouterActivityEvent }) {
  const outcome = event.kind === "tool_result"
    ? event.ok == null ? "Unavailable" : event.ok ? "Success" : "Failure"
    : null;
  const tone = event.kind === "tool_result" && event.ok != null
    ? event.ok ? "bg-ok" : "bg-danger"
    : "bg-border";
  return (
    <details className="group rounded-lg border border-border bg-surface-muted/30">
      <summary className="flex cursor-pointer flex-wrap items-center gap-2 px-3 py-2">
        <span className={`inline-block h-1.5 w-1.5 shrink-0 rounded-full ${tone}`} aria-hidden />
        <span className="min-w-0 flex-1 break-words text-xs font-medium">
          {event.kind === "round"
            ? <>Router round {event.round} · {event.model ?? "Model unavailable"}</>
            : <>{event.kind === "tool_proposed" ? "Model proposed" : "Harness reported"}: {event.name}</>}
        </span>
        <span className="ml-auto text-[11px] text-foreground-muted">
          {event.kind === "round" ? `${event.ms} ms` : outcome ?? "Proposal only"} · Sequence {event.seq}
        </span>
      </summary>
      <div className="space-y-1 border-t border-border px-3 py-2 text-xs text-foreground-muted">
        {event.kind === "round" ? (
          <>
            <p>Provider: {event.provider} · HTTP: {measured(event.http_status)} · Outcome: {event.outcome} · {event.ms} ms</p>
            <p>Tokens — input: {measured(event.usage?.prompt_tokens)} · output: {measured(event.usage?.completion_tokens)} · total: {measured(event.usage?.total_tokens)} · usage: {event.usage_state}</p>
            <p>Tool proposals observed: {event.tool_calls_observed} · Finish: {event.finish_reason ?? "Unavailable"}{event.partial_observation ? " · Partial observation" : ""}</p>
          </>
        ) : (
          <>
            <p>Proposed in router round {event.round}
              {event.kind === "tool_result" && <> · Reported in router round {event.reported_in_round} · Reported outcome: {outcome}</>}
              {event.kind === "tool_proposed" && " · Proposal only; execution not established"}
            </p>
            <p className="break-all font-mono">Call: {event.call_id}</p>
            {event.kind === "tool_result" && <p>{toolResultSummary(event.result_summary)}{event.ms !== undefined && ` Runtime reported duration: ${event.ms} ms.`}</p>}
          </>
        )}
        <p>Source: {event.source} · {event.observed_at
          ? <>Router observed at <time dateTime={event.observed_at}>{event.observed_at}</time></>
          : "Timestamp not retained by this recorder version"}</p>
      </div>
    </details>
  );
}

export function RouterActivityPanel({ binding, ns, name, focusQuery = "" }: {
  binding: BoundRouterActivity;
  ns?: string;
  name?: string;
  focusQuery?: string;
}) {
  const { observations: activity, producer, provisional } = binding;
  let selected: string[] | null = null;
  if (focusQuery.startsWith("observations:")) {
    try {
      const params = new URLSearchParams(focusQuery.slice("observations:".length));
      selected = [];
      if (params.size === 1 && params.has("producer")) {
        if (params.get("producer") === routerProducerKey(producer)) selected = null;
      } else if (params.size === 1 && params.has("ids")) {
        const ids: unknown = JSON.parse(params.get("ids")!);
        if (Array.isArray(ids) && ids.every(id => typeof id === "string")) selected = ids;
      }
    } catch { selected = []; }
  }
  const events = activity.state === "observed" ? activity.trace.events.filter(event =>
    selected === null || selected.includes(routerObservationKey(producer, event))) : [];
  const groups = new Map<number, RouterActivityEvent[]>();
  for (const event of events) {
    const group = groups.get(event.round) ?? [];
    group.push(event);
    groups.set(event.round, group);
  }
  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <h2 className="text-sm font-semibold">Router observations</h2>
      <p className="mt-2 text-sm">{provisional ? "Live, provisional observations from " : "Returned with this completed revision by "}{producer.agent_name}.</p>
      {provisional && <p className="mt-1 text-xs text-foreground-muted">Updated after correlated model responses. Not a completion record or a downloadable deliverable.</p>}
      <p className="mt-1 break-all font-mono text-xs text-foreground-muted">Run: {producer.run_nonce} · Assignment: {producer.assignment_id}</p>
      <p className="mt-1 break-all font-mono text-xs text-foreground-muted">Producer: {producer.agent_did}</p>
      <p className="mt-2 text-xs text-foreground-muted">
        Runtime-forwarded records from the shared inference router, selected by returned response IDs.
        These are partial observations, not independent proof of tool execution, artifact authorship or approval.
        Router round IDs identify model requests, not necessarily agent turns. Hidden retries and responses without IDs are not covered.
        {" "}Timestamps identify router observations, not exact tool start times. Raw arguments and result contents are excluded from telemetry; exported deliverables remain in Files.
      </p>
      <p className="mt-2 text-xs text-foreground-muted">
        Responses received: {activity.responses} · Correlated router rounds: {activity.rounds.length}
      </p>
      {activity.state === "unavailable" ? (
        <p className="mt-4 text-sm">Router observations unavailable: {activity.reason}</p>
      ) : (
        <>
          {(activity.trace.missing_rounds.length > 0 || activity.trace.dropped_events > 0 || activity.trace.truncated) && (
            <p className="mt-3 text-xs text-foreground-muted">
              Coverage gaps — missing rounds: {activity.trace.missing_rounds.join(", ") || "none reported"} · Recorder-wide dropped events: {activity.trace.dropped_events} · Selection truncated: {activity.trace.truncated ? "yes" : "no"}
            </p>
          )}
          {events.length === 0 ? (
            <p className="mt-4 text-sm">No selected records remain available. This does not establish that no work occurred.</p>
          ) : (
            <ol aria-label="Router rounds" className="mt-4 space-y-3">
              {[...groups].sort(([a], [b]) => a - b).map(([round, records]) => {
                const rounds = records.filter(event => event.kind === "round");
                const tools = records.filter(event => event.kind !== "round");
                return (
                  <li key={round}>
                    {rounds.length > 0 ? rounds.map(event => (
                      <Observation key={routerObservationKey(producer, event)} event={event} />
                    )) : (
                      <p className="px-3 py-1.5 text-xs text-foreground-muted">
                        Proposing round {round} · {selected === null ? "round record unavailable" : "round record not selected"}
                      </p>
                    )}
                    {tools.length > 0 && (
                      <ol aria-label={`Tools proposed in router round ${round}`} className="ml-[7px] mt-1 space-y-1 border-l border-border pl-3">
                        {tools.map(event => (
                          <li key={routerObservationKey(producer, event)}><Observation event={event} /></li>
                        ))}
                      </ol>
                    )}
                  </li>
                );
              })}
            </ol>
          )}
        </>
      )}
      {!provisional && ns && name && producer.run_nonce && <a
        href={revisionArtifactUrl(ns, name, ROUTER_OBSERVATIONS_ARTIFACT, producer.run_nonce)}
        download={ROUTER_OBSERVATIONS_ARTIFACT}
        className="mt-4 inline-block text-xs font-medium text-signal hover:underline"
      >Download system evidence</a>}
    </section>
  );
}
