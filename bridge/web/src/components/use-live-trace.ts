// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

import { useEffect, useMemo, useState } from "react";
import type { ActivityEvent } from "@/lib/types";
import { activityForRevision, mergeRevisionActivity, revisionStreamUrl } from "@/lib/mission-run-evidence";
import { advancesRouterActivity, liveRouterActivity, ReviewedActivityPins, type LiveRouterActivity } from "@/lib/router-activity";

/** One revision-pinned connection shared by the graph and activity feed. */
export function useLiveTrace(
  ns: string | undefined,
  name: string | undefined,
  running: boolean,
  seed: ActivityEvent[],
  runNonce?: string | null,
): { events: ActivityEvent[]; routerActivity: LiveRouterActivity | null; phaseActivity: Record<string, LiveRouterActivity> } {
  const url = revisionStreamUrl(ns, name, runNonce);
  const [live, setLive] = useState<{ key: string; events: ActivityEvent[]; routerActivity: LiveRouterActivity | null; phaseActivity: Record<string, LiveRouterActivity> } | null>(null);
  // The first client render must match the server seed during hydration.
  const [mounted, setMounted] = useState(false);
  useEffect(() => {
    const frame = requestAnimationFrame(() => setMounted(true));
    return () => cancelAnimationFrame(frame);
  }, []);

  useEffect(() => {
    if (!running || !url || !runNonce) return;
    let active = true;
    const es = new EventSource(url);
    const close = () => { active = false; es.close(); };
    let pinned: LiveRouterActivity | null = null;
    let lastObserved: LiveRouterActivity | null = null;
    const phases = new ReviewedActivityPins();
    const clearProvisional = () => {
      if (!active) return;
      setLive(previous => previous?.key === url ? { ...previous, routerActivity: null } : previous);
    };
    const clearAllProvisional = () => {
      if (!active) return;
      phases.clearVisible();
      setLive(previous => previous?.key === url ? { ...previous, routerActivity: null, phaseActivity: {} } : previous);
    };
    const clearAndClose = () => {
      clearAllProvisional();
      close();
    };
    // Native reconnect keeps the same custody pins, but disconnected evidence
    // must stay hidden until the BFF publishes another validated snapshot.
    es.addEventListener("error", clearAllProvisional);
    es.addEventListener("renew", clearAllProvisional);
    es.addEventListener("unavailable", clearProvisional);
    es.addEventListener("reviewed-phase-activity", message => {
      if (!active) return;
      try {
        if (!phases.observe(JSON.parse((message as MessageEvent).data), runNonce)) { clearAndClose(); return; }
        setLive(previous => ({
          key: url,
          events: previous?.key === url ? previous.events : [],
          routerActivity: previous?.key === url ? previous.routerActivity : null,
          phaseActivity: { ...phases.current },
        }));
      } catch {
        clearAndClose();
      }
    });
    es.addEventListener("router-activity", message => {
      if (!active) return;
      try {
        const next = liveRouterActivity(JSON.parse((message as MessageEvent).data), runNonce);
        if (!next || !advancesRouterActivity(pinned, next) || !advancesRouterActivity(lastObserved, next)) { clearAndClose(); return; }
        pinned = next;
        if (next.observations.state === "observed") lastObserved = next;
        setLive(previous => ({
          key: url,
          events: previous?.key === url ? previous.events : [],
          routerActivity: next,
          phaseActivity: previous?.key === url ? previous.phaseActivity : {},
        }));
      } catch {
        clearAndClose();
      }
    });
    es.addEventListener("router-activity-unavailable", clearAndClose);
    es.onmessage = (message) => {
      if (!active) return;
      try {
        const event: unknown = JSON.parse(message.data);
        if (!activityForRevision(event, runNonce)) return;
        setLive(previous => ({
          key: url,
          events: mergeRevisionActivity(previous?.key === url ? previous.events : [], [event], runNonce),
          routerActivity: previous?.key === url ? previous.routerActivity : null,
          phaseActivity: previous?.key === url ? previous.phaseActivity : {},
        }));
      } catch {
        // Malformed frames are not activity evidence.
      }
    };
    es.addEventListener("done", clearAndClose);
    es.addEventListener("end", clearAndClose);
    return clearAndClose;
  }, [running, url, runNonce]);

  return useMemo(() => ({
    events: mergeRevisionActivity(seed, mounted && url && live?.key === url ? live.events : [], runNonce),
    routerActivity: mounted && running && url && live?.key === url ? live.routerActivity : null,
    phaseActivity: mounted && running && url && live?.key === url ? live.phaseActivity : {},
  }), [seed, live, mounted, running, url, runNonce]);
}
