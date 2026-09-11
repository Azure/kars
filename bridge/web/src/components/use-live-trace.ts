"use client";

// kars Bridge — shared live-activity stream hook.
//
// One EventSource per mission/team run, consumed by BOTH the agent graph and the
// activity feed so a single Activity tab never opens two connections. It:
//  - resets when the task `name` changes (no cross-run event contamination),
//  - de-duplicates by a composite key (agent+kind+seq+round) so a server-side
//    seed that already contains live events, plus the SSE tail, never double-
//    count the same event,
//  - does NOT close on a transient error (EventSource auto-reconnects); it only
//    closes on the terminal `done` event and on unmount.

import { useEffect, useMemo, useRef, useState } from "react";
import type { ActivityEvent } from "@/lib/types";

function eventKey(e: ActivityEvent, principal: string, idx: number): string {
  const anyE = e as unknown as Record<string, unknown>;
  const seq = anyE.seq;
  // When the router stamped a `seq`, it uniquely identifies the event, and the
  // SAME event appears once from the seed (agent normalized to the principal —
  // the seed IS the principal's trace) and once from the live tail (agent = the
  // emitting sandbox). Key on (agent, seq) so those de-dupe while running.
  if (typeof seq === "number") {
    const rawInstance = anyE.agentInstance;
    const rawAgent = anyE.agent;
    const agent =
      typeof rawInstance === "string" && rawInstance
        ? rawInstance
        : typeof rawAgent === "string" && rawAgent
          ? rawAgent
          : principal;
    return `${agent}:${e.kind}:${seq}`;
  }
  // No router `seq` — this is the persisted, agent-self-reported trace read on a
  // DELIVERED run (no live tail to collide with). Its events are already unique,
  // so key on the source index to keep every round/tool distinct. (Dropping this
  // and keying only on agent:kind collapsed a 6-round run to a single row.)
  const round = "round" in anyE ? anyE.round : "";
  return `seed:${e.kind}:${round}:${idx}`;
}

/**
 * Returns the de-duplicated union of the server-provided `seed` activity and the
 * live SSE tail. One connection, shared by every consumer that passes the result
 * down as `events`.
 */
export function useLiveTrace(
  ns: string | undefined,
  name: string | undefined,
  running: boolean,
  seed: ActivityEvent[],
): ActivityEvent[] {
  const [live, setLive] = useState<ActivityEvent[]>([]);
  // Gate live events behind a mount flag so the FIRST client render is byte-for-
  // byte identical to the server's (both derive purely from `seed`). Without
  // this, an EventSource frame that lands between hydration scheduling and commit
  // can slip a live event into the first client render, diverging from the SSR
  // HTML and tripping a hydration mismatch in every consumer (graph + feed).
  const [mounted, setMounted] = useState(false);
  useEffect(() => {
    const frame = requestAnimationFrame(() => setMounted(true));
    return () => cancelAnimationFrame(frame);
  }, []);
  // Track the current stream key so a `name` change resets accumulated events.
  const streamKey = `${ns ?? ""}/${name ?? ""}`;
  const prevKey = useRef(streamKey);

  useEffect(() => {
    // New task → drop any events accumulated for the previous one.
    if (prevKey.current !== streamKey) {
      prevKey.current = streamKey;
      setLive([]);
    }
    if (!running || !ns || !name) return;
    const es = new EventSource(`/api/namespaces/${ns}/tasks/${name}/stream`);
    es.onmessage = (m) => {
      try {
        setLive((prev) => [...prev, JSON.parse(m.data) as ActivityEvent]);
      } catch {
        /* ignore malformed frame */
      }
    };
    es.addEventListener("done", () => es.close());
    // Intentionally NOT closing on error: the browser auto-reconnects an
    // EventSource, so a transient blip resumes instead of stranding the stream.
    return () => es.close();
  }, [running, ns, name, streamKey]);

  return useMemo(() => {
    const map = new Map<string, ActivityEvent>();
    const principal = name ?? "";
    seed.forEach((e, i) => map.set(eventKey(e, principal, i), e));
    if (mounted) live.forEach((e, i) => map.set(eventKey(e, principal, seed.length + i), e));
    return [...map.values()];
  }, [seed, live, mounted, name]);
}
