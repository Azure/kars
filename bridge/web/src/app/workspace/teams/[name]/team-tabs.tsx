"use client";

// kars Bridge — Team console tabs. Tames the standing-team monolith: instead of
// eight stacked full-width sections, the body is organised into tabs (Overview /
// Org & access / Runs / Knowledge / Ledger). The header + "Now" hero stay above
// as a persistent summary. Each tab is server-rendered and passed in as a node.
// We mount only the tabs the user has actually opened (lazy), so a heavy tab —
// e.g. Knowledge with dozens of entry bodies — costs nothing in the DOM until
// viewed, then stays mounted to preserve scroll/interaction. Mirrors MissionTabs.

import { useState, type KeyboardEvent, type ReactNode } from "react";

export type TeamTab = {
  id: string;
  label: string;
  badge?: number | string | null;
  node: ReactNode;
  live?: boolean;
};

export function TeamTabs({ tabs, initial }: { tabs: TeamTab[]; initial?: string }) {
  const visible = tabs.filter((t) => t.node);
  const first = initial && visible.some((t) => t.id === initial) ? initial : visible[0]?.id;
  const [active, setActive] = useState(first);
  // If the active tab is no longer present (e.g. a live run's Activity tab
  // disappears when the run ends), fall back to the first visible tab instead of
  // rendering an empty body.
  const activeId = visible.some((t) => t.id === active) ? active : visible[0]?.id;
  // Track which tabs have been opened so we mount their node lazily (on first
  // view) and keep it mounted thereafter — a heavy tab doesn't hit the DOM until
  // the user actually navigates to it.
  const [opened, setOpened] = useState<Set<string>>(() => new Set(first ? [first] : []));
  const open = (id: string) => {
    setActive(id);
    setOpened((prev) => (prev.has(id) ? prev : new Set(prev).add(id)));
  };
  // Arrow-key roving-tab navigation (WAI-ARIA tabs pattern).
  const onKey = (e: KeyboardEvent) => {
    const idx = visible.findIndex((t) => t.id === activeId);
    if (idx < 0) return;
    let next = idx;
    if (e.key === "ArrowRight" || e.key === "ArrowDown") next = (idx + 1) % visible.length;
    else if (e.key === "ArrowLeft" || e.key === "ArrowUp") next = (idx - 1 + visible.length) % visible.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = visible.length - 1;
    else return;
    e.preventDefault();
    const id = visible[next]?.id;
    if (id) {
      open(id);
      document.getElementById(`teamtab-${id}`)?.focus();
    }
  };
  return (
    <div>
      <div
        role="tablist"
        aria-label="Team sections"
        onKeyDown={onKey}
        className="sticky top-[57px] z-10 -mx-1 mb-5 flex gap-1 overflow-x-auto rounded-xl border border-border bg-surface/80 p-1 backdrop-blur supports-[backdrop-filter]:bg-surface/70"
      >
        {visible.map((t) => {
          const on = t.id === activeId;
          return (
            <button
              key={t.id}
              type="button"
              role="tab"
              id={`teamtab-${t.id}`}
              aria-selected={on}
              aria-controls={`teamtabpanel-${t.id}`}
              tabIndex={on ? 0 : -1}
              onClick={() => open(t.id)}
              className={`relative flex shrink-0 items-center gap-1.5 rounded-lg px-3.5 py-1.5 text-sm font-medium transition ${on ? "bg-signal/10 text-foreground" : "text-foreground-muted hover:bg-surface-muted hover:text-foreground"}`}
            >
              {t.live && <span className="h-1.5 w-1.5 rounded-full bg-signal kb-pulse" />}
              {t.label}
              {t.badge != null && t.badge !== 0 && (
                <span
                  className={`rounded-full px-1.5 text-[11px] tabular-nums ${on ? "bg-signal/20 text-signal" : "bg-surface-muted text-foreground-muted"}`}
                >
                  {t.badge}
                </span>
              )}
            </button>
          );
        })}
      </div>
      <div className="kb-rise space-y-6">
        {visible.map((t) => (
          <div
            key={t.id}
            role="tabpanel"
            id={`teamtabpanel-${t.id}`}
            aria-labelledby={`teamtab-${t.id}`}
            hidden={t.id !== activeId}
          >
            {opened.has(t.id) ? t.node : null}
          </div>
        ))}
      </div>
    </div>
  );
}
