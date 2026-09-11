"use client";

// kars Bridge Workspace — live mission canvas.
//
// While a mission is live (its sandbox is running, or a run is in flight) the
// server component is re-fetched on a short interval so the activity trace,
// token telemetry, deliverable, and artifact set appear as the run lands — no
// manual reload. `router.refresh()` re-runs the server render with fresh BFF
// data while preserving client state. Polling stops the moment the mission is
// no longer active or the page is hidden, so it never spins needlessly.

import { useEffect, useRef, useState } from "react";
import { useRouter } from "next/navigation";

export function LiveRefresh({
  active,
  intervalMs = 3500,
}: {
  active: boolean;
  intervalMs?: number;
}) {
  const router = useRouter();
  const timer = useRef<ReturnType<typeof setInterval> | null>(null);
  // Guard against overlapping refreshes: if the BFF is slower than the interval,
  // don't stack a second router.refresh() on top of an in-flight one (which can
  // deliver stale-after-fresh out of order). We settle the guard after a short
  // window that comfortably exceeds a normal server round-trip.
  const refreshing = useRef(false);

  useEffect(() => {
    if (!active) return;
    const tick = () => {
      if (document.visibilityState !== "visible" || refreshing.current) return;
      refreshing.current = true;
      router.refresh();
      setTimeout(() => {
        refreshing.current = false;
      }, Math.max(1000, intervalMs - 250));
    };
    // Refresh once immediately on mount so an operator arriving mid-run sees the
    // current state without waiting a full interval (or hitting F5), then poll.
    if (document.visibilityState === "visible") {
      refreshing.current = true;
      router.refresh();
      setTimeout(() => {
        refreshing.current = false;
      }, Math.max(1000, intervalMs - 250));
    }
    timer.current = setInterval(tick, intervalMs);
    return () => {
      if (timer.current) clearInterval(timer.current);
      refreshing.current = false;
    };
  }, [active, intervalMs, router]);

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
