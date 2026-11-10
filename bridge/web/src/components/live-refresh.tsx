// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

// kars Bridge Workspace — live mission canvas.
//
// While a mission is live (its sandbox is running, or a run is in flight) the
// server component is re-fetched on a short interval so the activity trace,
// token telemetry, deliverable, and artifact set appear as the run lands — no
// manual reload. `router.refresh()` re-runs the server render with fresh BFF
// data while preserving client state. Polling stops the moment the mission is
// no longer active or the page is hidden. Returning to an idle tab still
// refreshes once, since another tab or operator may have changed its state.

import { useEffect, useRef, useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import { hasPendingNavigation } from "@/components/navigation-link";

// A page can have both lifecycle acknowledgement and general activity polling.
let refreshOwner: symbol | undefined;

export function LiveRefresh({
  active,
  intervalMs = 3500,
}: {
  active: boolean;
  intervalMs?: number;
}) {
  const router = useRouter();
  const [refreshPending, startRefresh] = useTransition();
  const owner = useRef(Symbol());
  useEffect(() => {
    if (!refreshPending && refreshOwner === owner.current) refreshOwner = undefined;
  });
  useEffect(() => {
    const token = owner.current;
    return () => { if (refreshOwner === token) refreshOwner = undefined; };
  }, []);
  useEffect(() => {
    let coolingDown = false;
    let cooldown: ReturnType<typeof setTimeout> | undefined;
    const tick = () => {
      if (document.visibilityState !== "visible" || coolingDown || refreshOwner !== undefined || hasPendingNavigation()) return;
      coolingDown = true;
      refreshOwner = owner.current;
      startRefresh(() => router.refresh());
      cooldown = setTimeout(() => {
        coolingDown = false;
      }, Math.max(1000, intervalMs - 250));
    };
    if (active) tick();
    const timer = active ? setInterval(tick, intervalMs) : undefined;
    window.addEventListener("focus", tick);
    document.addEventListener("visibilitychange", tick);
    return () => {
      if (timer !== undefined) clearInterval(timer);
      if (cooldown !== undefined) clearTimeout(cooldown);
      window.removeEventListener("focus", tick);
      document.removeEventListener("visibilitychange", tick);
    };
  }, [active, intervalMs, router, startRefresh]);

  return null;
}

/// A small pulsing "live" indicator — a calm signal that the canvas is
/// auto-updating. Purely presentational.
export function LivePulse({ label = "Live" }: { label?: string }) {
  return (
    <span className="inline-flex items-center gap-1.5 rounded-full bg-signal/10 px-2.5 py-1 text-xs font-medium text-signal">
      <span className="relative flex h-2 w-2">
        <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-signal/70" />
        <span className="relative inline-flex h-2 w-2 rounded-full bg-signal" />
      </span>
      {label}
    </span>
  );
}

/// Wraps any content and gently fades/slides it in on mount — used so newly
/// arrived activity rows feel alive rather than popping in abruptly.
export function FadeIn({ children }: { children: React.ReactNode }) {
  const [shown, setShown] = useState(false);
  useEffect(() => {
    const id = requestAnimationFrame(() => setShown(true));
    return () => cancelAnimationFrame(id);
  }, []);
  return (
    <div
      className={`transition-all duration-300 ${shown ? "translate-y-0 opacity-100" : "translate-y-1 opacity-0"}`}
    >
      {children}
    </div>
  );
}
