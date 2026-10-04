// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { revisionArtifactUrl } from "@/lib/mission-run-evidence";
import { ROUTER_OBSERVATIONS_ARTIFACT, type RouterActivity, type RouterActivityEvent } from "@/lib/router-activity";

const measured = (value: number | null | undefined) => value == null ? "Unavailable" : String(value);

function Observation({ event }: { event: RouterActivityEvent }) {
  if (event.kind === "round") {
    return (
      <>
        <h3 className="text-sm font-medium">Router round {event.round} · {event.model ?? "Model unavailable"}</h3>
        <p className="mt-1 text-xs text-foreground-muted">
          Provider: {event.provider} · HTTP: {measured(event.http_status)} · Outcome: {event.outcome} · {event.ms} ms
        </p>
        <p className="mt-1 text-xs text-foreground-muted">
          Tokens — input: {measured(event.usage?.prompt_tokens)} · output: {measured(event.usage?.completion_tokens)} · total: {measured(event.usage?.total_tokens)} · usage: {event.usage_state}
        </p>
        <p className="mt-1 text-xs text-foreground-muted">
          Tool proposals observed: {event.tool_calls_observed} · Finish: {event.finish_reason ?? "Unavailable"}
          {event.partial_observation ? " · Partial observation" : ""}
        </p>
      </>
    );
  }
  return (
    <>
      <h3 className="text-sm font-medium">{event.kind === "tool_proposed" ? "Model proposed" : "Harness reported"}: {event.name}</h3>
      <p className="mt-1 text-xs text-foreground-muted">
        Proposed in router round {event.round}
        {event.kind === "tool_result" && <> · Reported in router round {event.reported_in_round} · Reported outcome: {event.ok == null ? "Unavailable" : event.ok ? "Success" : "Failure"}</>}
        {event.kind === "tool_proposed" && " · Proposal only; execution not established"}
      </p>
      <p className="mt-1 break-all font-mono text-xs text-foreground-muted">Call: {event.call_id}</p>
    </>
  );
}

export function RouterActivityPanel({ activity, producer, ns, name, runNonce }: {
  activity: RouterActivity;
  producer: string;
  ns: string;
  name: string;
  runNonce: string;
}) {
  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <h2 className="text-sm font-semibold">Router observations</h2>
      <p className="mt-2 text-sm">Returned by {producer} with this completed revision.</p>
      <p className="mt-1 break-all font-mono text-xs text-foreground-muted">Run: {runNonce}</p>
      <p className="mt-2 text-xs text-foreground-muted">
        Runtime-forwarded records from the shared inference router, selected by returned response IDs.
        These are partial observations, not independent proof of tool execution, artifact authorship or approval.
        Router round IDs identify model requests, not necessarily agent turns. Hidden retries and responses without IDs are not covered.
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
          {activity.trace.events.length === 0 ? (
            <p className="mt-4 text-sm">No selected records remain available. This does not establish that no work occurred.</p>
          ) : (
            <ol className="mt-4 divide-y divide-border rounded-lg border border-border">
              {activity.trace.events.map((event) => (
                <li key={event.seq} className="px-4 py-3"><Observation event={event} /></li>
              ))}
            </ol>
          )}
        </>
      )}
      <a
        href={revisionArtifactUrl(ns, name, ROUTER_OBSERVATIONS_ARTIFACT, runNonce)}
        aria-disabled={!runNonce}
        download={ROUTER_OBSERVATIONS_ARTIFACT}
        className="mt-4 inline-block text-xs font-medium text-signal hover:underline"
      >Download system evidence</a>
    </section>
  );
}
