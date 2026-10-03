// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

import { useEffect, useMemo, useState } from "react";
import type { ActivityEvent } from "@/lib/types";
import { activityForRevision, mergeRevisionActivity, revisionStreamUrl } from "@/lib/mission-run-evidence";

/** One revision-pinned connection shared by the graph and activity feed. */
export function useLiveTrace(
  ns: string | undefined,
  name: string | undefined,
  running: boolean,
  seed: ActivityEvent[],
  runNonce?: string | null,
): ActivityEvent[] {
  const url = revisionStreamUrl(ns, name, runNonce);
  const [live, setLive] = useState<{ key: string; events: ActivityEvent[] } | null>(null);
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
    es.onmessage = (message) => {
      if (!active) return;
      try {
        const event: unknown = JSON.parse(message.data);
        if (!activityForRevision(event, runNonce)) return;
        setLive(previous => ({
          key: url,
          events: mergeRevisionActivity(previous?.key === url ? previous.events : [], [event], runNonce),
        }));
      } catch {
        // Malformed frames are not activity evidence.
      }
    };
    es.addEventListener("done", close);
    es.addEventListener("end", close);
    return close;
  }, [running, url, runNonce]);

  return useMemo(() => mergeRevisionActivity(
    seed,
    mounted && url && live?.key === url ? live.events : [],
    runNonce,
  ), [seed, live, mounted, url, runNonce]);
}
