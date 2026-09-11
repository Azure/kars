"use client";

// Unified intent-first intake. One box: describe the outcome. Bridge classifies
// it as a one-off mission or a standing team, shows the recommendation with a
// plain-language reason, and lets you flip it before continuing. Continuing
// routes to the matching composer with the intent prefilled — which then
// auto-composes so you land on the editable package/org chart, not a blank box.
// One intent → smart routing → review → pre-flight → launch: a single flow.

import { useMemo, useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import { classifyIntent, type IntentKind } from "@/lib/classify-intent";
import { LoopDesigner } from "@/components/loop-designer";
import { Icon } from "@/components/icon";

const EXAMPLES: { label: string; text: string; kind: IntentKind }[] = [
  {
    label: "Research report",
    text: "Research the current state of open-source agent frameworks and write a report comparing their governance models.",
    kind: "mission",
  },
  {
    label: "Repo health team",
    text: "Keep the kars repo healthy: triage new issues, watch open PRs, and report failing checks to me daily.",
    kind: "team",
  },
  {
    label: "Competitor watch",
    text: "Monitor our top 3 competitors' product launches continuously and summarize anything material every week.",
    kind: "team",
  },
];

export function IntentEntry() {
  const router = useRouter();
  const [intent, setIntent] = useState("");
  const [override, setOverride] = useState<IntentKind | null>(null);
  const [pending, startTransition] = useTransition();

  const auto = useMemo(() => classifyIntent(intent), [intent]);
  const kind: IntentKind = override ?? auto.kind;
  const ready = intent.trim().length >= 8;

  function go() {
    if (!ready) return;
    const q = `intent=${encodeURIComponent(intent.trim())}`;
    const href = kind === "team" ? `/workspace/teams/new?${q}` : `/workspace/new?${q}`;
    startTransition(() => router.push(href));
  }

  return (
    <div>
      <textarea
        value={intent}
        onChange={(e) => {
          setIntent(e.target.value);
          setOverride(null);
        }}
        onKeyDown={(e) => {
          if ((e.metaKey || e.ctrlKey) && e.key === "Enter") go();
        }}
        rows={3}
        autoFocus
        placeholder="e.g. Research the top agentic-AI launches this quarter and write me a briefing — or — keep an eye on our repo's PRs and tell me when checks fail."
        className="w-full resize-y rounded-2xl border border-border bg-surface px-4 py-3 text-sm leading-relaxed shadow-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
      />

      {/* Live recommendation — transparent, always overridable. */}
      {ready && (
        <div className="mt-3 flex flex-col gap-2 rounded-xl border border-border bg-surface-muted/40 p-3 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex items-start gap-2.5">
            <span
              className={`grid h-8 w-8 shrink-0 place-items-center rounded-lg text-lg ${
                kind === "team" ? "bg-accent/15 text-accent" : "bg-signal/15 text-signal"
              }`}
              aria-hidden
            >
              {kind === "team" ? <Icon name="handshake" size={18} /> : <Icon name="target" size={18} />}
            </span>
            <div className="min-w-0">
              <p className="text-sm font-semibold">
                {kind === "team" ? "Recommended: a standing team" : "Recommended: a one-off mission"}
                {!override && (
                  <span className="ml-2 rounded-full bg-surface px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide text-foreground-muted">
                    {auto.confidence} confidence
                  </span>
                )}
              </p>
              <p className="mt-0.5 text-xs text-foreground-muted">
                {override ? "You chose this manually." : auto.reason}
              </p>
            </div>
          </div>
          <div className="flex shrink-0 items-center gap-1.5 rounded-lg border border-border bg-surface p-1 text-xs">
            <button
              type="button"
              onClick={() => setOverride("mission")}
              className={`rounded-md px-2.5 py-1 font-medium transition ${
                kind === "mission" ? "bg-signal text-signal-fg" : "text-foreground-muted hover:text-foreground"
              }`}
            >
              Mission
            </button>
            <button
              type="button"
              onClick={() => setOverride("team")}
              className={`rounded-md px-2.5 py-1 font-medium transition ${
                kind === "team" ? "bg-accent text-accent-fg" : "text-foreground-muted hover:text-foreground"
              }`}
            >
              Team
            </button>
          </div>
        </div>
      )}

      <div className="mt-3 flex flex-wrap items-center gap-2">
        <button
          type="button"
          onClick={go}
          disabled={!ready || pending}
          className="inline-flex items-center gap-2 rounded-lg bg-signal px-5 py-2.5 text-sm font-semibold text-signal-fg disabled:opacity-50"
        >
          {pending ? "Composing…" : kind === "team" ? "Compose the team →" : "Compose the mission →"}
        </button>
        <span className="text-xs text-foreground-muted">
          ⌘↵ to continue · you review &amp; adjust everything before anything runs
        </span>
      </div>

      {/* Examples — one-click seeds that also demonstrate the routing. */}
      <div className="mt-4 flex flex-wrap gap-2">
        <span className="text-[11px] font-medium uppercase tracking-wide text-foreground-muted">Try</span>
        {EXAMPLES.map((ex) => (
          <button
            key={ex.label}
            type="button"
            onClick={() => {
              setIntent(ex.text);
              setOverride(null);
            }}
            className="rounded-full border border-border bg-surface px-3 py-1 text-xs text-foreground-muted transition hover:border-signal hover:text-foreground"
          >
            {ex.label}
          </button>
        ))}
      </div>
    </div>
  );
}
