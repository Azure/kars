"use client";

// kars Bridge Workspace — first-run auto-kickoff.
//
// The §20 gate is two beats: Launch materialises the governed sandbox, then a
// run delivers the objective into it. An operator who launched a one-shot
// mission expects it to just start — they shouldn't have to click "Run" a
// second time once the sandbox is up. This tiny component drives that first run
// exactly once, the instant a launched mission's sandbox reaches Running with no
// prior run (no deliverable, no activity yet). Re-runs stay a deliberate "Run
// again" click in the Execution panel.
//
// It lives here (rendered unconditionally on the mission page) rather than in the
// Execution panel because the panel sits on the lazily-mounted Overview tab — a
// launched+running mission opens on the Activity tab, so the panel isn't mounted
// and its effect would never fire. The run endpoint's in-flight nonce guard makes
// a stray double-trigger a no-op, so being always-mounted is safe.

import { useEffect, useRef, useState } from "react";
import { useRouter } from "next/navigation";
import { runMissionClient } from "@/lib/run-mission-client";

export function MissionAutoRun({
  namespace,
  name,
  active,
}: {
  namespace: string;
  name: string;
  active: boolean;
}) {
  const router = useRouter();
  const fired = useRef(false);
  const inFlight = useRef(false);
  const [attempt, setAttempt] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const maxAttempts = 5;

  useEffect(() => {
    if (!active || fired.current || inFlight.current || attempt >= maxAttempts) return;
    let cancelled = false;
    let retry: ReturnType<typeof setTimeout> | null = null;
    inFlight.current = true;
    void (async () => {
      const result = await runMissionClient(namespace, name);
      inFlight.current = false;
      if (cancelled) return;
      if (result.ok) {
        fired.current = true;
        setError(null);
        router.refresh();
        return;
      }
      setError(result.error ?? "The automatic mission start failed.");
      router.refresh();
      retry = setTimeout(
        () => setAttempt((current) => current + 1),
        Math.min(2_000 * 2 ** attempt, 10_000),
      );
    })();
    return () => {
      cancelled = true;
      if (retry) clearTimeout(retry);
    };
  }, [active, attempt, maxAttempts, name, namespace, router]);

  if (!error) return null;
  const exhausted = attempt >= maxAttempts - 1;
  return (
    <div role="alert" className="rounded-xl border border-warning/40 bg-warning/10 px-4 py-3 text-sm">
      <p className="font-medium">
        {exhausted ? "The mission could not start automatically." : "The mission start is retrying."}
      </p>
      <p className="mt-1 text-xs text-foreground-muted">{error}</p>
      {exhausted && (
        <button
          type="button"
          onClick={() => {
            setError(null);
            setAttempt(0);
          }}
          className="mt-2 rounded-lg border border-border bg-surface px-3 py-1.5 text-xs font-medium hover:bg-surface-muted"
        >
          Retry start
        </button>
      )}
    </div>
  );
}
