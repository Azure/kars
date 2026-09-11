"use client";

// kars Bridge — the shared live Activity view. Opens ONE SSE connection (via
// useLiveTrace) and feeds both the auto-folding agent graph and the per-round /
// per-tool feed, so the Activity tab never opens two connections to the same
// stream. Missions and team runs both render through this.

import { useLiveTrace } from "./use-live-trace";
import { AgentGraph } from "./agent-graph";
import { ActivityStream } from "./activity-stream";
import type { ActivityEvent, MissionTelemetry, SubAgent } from "@/lib/types";

export function LiveActivityView({
  running,
  activity,
  telemetry,
  ns,
  name,
  agentLabel,
  subAgents = [],
  showGraph = true,
  identity = null,
  envelopeDigest = null,
  receipt = null,
  activityTitle,
  activityDetail,
}: {
  running: boolean;
  activity: ActivityEvent[];
  telemetry: MissionTelemetry | null;
  ns?: string;
  name?: string;
  agentLabel?: string;
  subAgents?: SubAgent[];
  showGraph?: boolean;
  identity?: import("@/lib/types").AgentIdentity | null;
  envelopeDigest?: string | null;
  receipt?: import("@/lib/types").Receipt | null;
  activityTitle?: string;
  activityDetail?: string;
}) {
  // One stream, shared by the graph and the feed.
  const events = useLiveTrace(ns, name, running, activity);
  return (
    <div className="space-y-4">
      {showGraph && (
        <AgentGraph
          running={running}
          activity={activity}
          events={events}
          ns={ns}
          name={name}
          agentLabel={agentLabel}
          subAgents={subAgents}
          identity={identity}
          envelopeDigest={envelopeDigest}
          receipt={receipt}
        />
      )}
      <ActivityStream
        running={running}
        activity={activity}
        events={events}
        telemetry={telemetry}
        ns={ns}
        name={name}
        title={activityTitle}
        detail={activityDetail}
      />
    </div>
  );
}
