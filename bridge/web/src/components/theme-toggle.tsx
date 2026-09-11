"use client";

// kars Bridge — theme toggle. The palette already ships light + dark token sets
// (globals.css); this lets an operator explicitly pick one instead of being
// locked to the OS preference. The choice is persisted in localStorage and
// applied by adding `light` / `dark` (or neither = follow system) to <html>.
// A tiny inline script in the root layout applies the saved choice before
// first paint so there is no flash of the wrong theme.

import { useEffect, useState } from "react";
import { Icon } from "@/components/icon";

type Choice = "light" | "dark" | "system";

const STORAGE_KEY = "kb-theme";

function apply(choice: Choice): void {
  const root = document.documentElement;
  root.classList.remove("light", "dark");
  if (choice === "light") root.classList.add("light");
  else if (choice === "dark") root.classList.add("dark");
  try {
    if (choice === "system") localStorage.removeItem(STORAGE_KEY);
    else localStorage.setItem(STORAGE_KEY, choice);
  } catch {
    /* private mode — non-fatal, the in-memory choice still applies */
  }
}

function current(): Choice {
  if (typeof document === "undefined") return "system";
  const root = document.documentElement;
  if (root.classList.contains("dark")) return "dark";
  if (root.classList.contains("light")) return "light";
  return "system";
}

export function ThemeToggle() {
  // Cycle light -> dark -> system so all three are reachable from one control.
  const [choice, setChoice] = useState<Choice>("system");

  useEffect(() => {
    const frame = window.requestAnimationFrame(() => setChoice(current()));
    return () => window.cancelAnimationFrame(frame);
  }, []);

  const next: Record<Choice, Choice> = { light: "dark", dark: "system", system: "light" };
  // What the CURRENT theme is (for the icon + the accessible "current" hint).
  const label: Record<Choice, string> = {
    light: "Light",
    dark: "Dark",
    system: "System",
  };
  // What CLICKING does — the button describes the ACTION, not the current state,
  // so "Use dark theme" actually switches to dark (audit N3).
  const actionLabel: Record<Choice, string> = {
    light: "Use dark theme",
    dark: "Use system theme",
    system: "Use light theme",
  };
  const iconFor: Record<Choice, import("@/components/icon").IconName> = {
    light: "target",
    dark: "shield",
    system: "gear",
  };

  return (
    <button
      type="button"
      onClick={() => {
        const c = next[choice];
        setChoice(c);
        apply(c);
      }}
      title={`${label[choice]} theme active — click to ${actionLabel[choice].toLowerCase()}`}
      aria-label={`${label[choice]} theme active. ${actionLabel[choice]}.`}
      className="inline-flex items-center gap-1.5 rounded-lg border border-border bg-surface px-2.5 py-1.5 text-xs font-medium text-foreground-muted transition hover:bg-surface-muted hover:text-foreground"
    >
      <Icon name={iconFor[choice]} className="h-3.5 w-3.5" aria-hidden />
      <span className="hidden sm:inline">{actionLabel[choice]}</span>
    </button>
  );
}
