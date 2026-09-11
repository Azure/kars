"use client";

// kars Bridge Workspace — primary navigation (employee surface).
// Plain-language, mission-first. No Kubernetes vocabulary ever.

import Link from "next/link";
import { usePathname } from "next/navigation";

const NAV = [
  { href: "/workspace", label: "Home", exact: true, icon: "home", hint: "Start here" },
  { href: "/workspace/missions", label: "Missions", exact: false, icon: "grid", hint: "One-off tasks" },
  { href: "/workspace/agents", label: "Active agents", exact: false, icon: "pulse", hint: "Working right now" },
  { href: "/workspace/teams", label: "Teams", exact: false, icon: "people", hint: "Standing, long-running work" },
  { href: "/workspace/inbox", label: "Inbox", exact: false, icon: "inbox", hint: "Decisions waiting on you" },
  { href: "/workspace/artifacts", label: "Artifacts", exact: false, icon: "doc", hint: "What was produced" },
  { href: "/workspace/skills", label: "Skills", exact: false, icon: "doc", hint: "Upload & assign capabilities" },
  { href: "/workspace/connections", label: "Connections", exact: false, icon: "link", hint: "Connect your GitHub repos" },
] as const;

const ICON: Record<string, React.ReactNode> = {
  home: <path d="M3 9.5 10 4l7 5.5V17a1 1 0 0 1-1 1h-3v-5H7v5H4a1 1 0 0 1-1-1V9.5Z" />,
  plus: <path d="M10 4v12M4 10h12" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" fill="none" />,
  grid: <path d="M3 3h6v6H3V3Zm8 0h6v6h-6V3ZM3 11h6v6H3v-6Zm8 0h6v6h-6v-6Z" />,
  people: <path d="M7 9a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5Zm6 0a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5Zm-6 1.5c-2.5 0-4.5 1.4-4.5 3.2V16h9v-2.3c0-1.8-2-3.2-4.5-3.2Zm6 0c-.6 0-1.2.08-1.7.23 1 .8 1.7 1.9 1.7 3v2.27h4.5V13.7c0-1.8-2-3.2-4.5-3.2Z" />,
  inbox: <path d="M3 4h14v9a1 1 0 0 1-1 1h-3l-1 2H8l-1-2H4a1 1 0 0 1-1-1V4Zm2 2v5h2l1 2h4l1-2h2V6H5Z" />,
  doc: <path d="M5 2h7l3 3v13a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V3a1 1 0 0 1 1-1Zm6 1.5V6h2.5L11 3.5Z" />,
  chart: <path d="M3 17h14M6 13v2M10 8v7M14 11v4" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" fill="none" />,
  pulse: <path d="M2 10h4l2-5 4 10 2-5h4" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" fill="none" />,
  link: <path d="M8 12a3 3 0 0 0 4 0l2-2a3 3 0 0 0-4-4l-1 1M12 8a3 3 0 0 0-4 0l-2 2a3 3 0 0 0 4 4l1-1" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" fill="none" />,
};

export function WorkspaceNav({ pendingAsks = 0 }: { pendingAsks?: number }) {
  const pathname = usePathname();
  return (
    <nav aria-label="Workspace" className="hidden w-52 shrink-0 md:block">
      <ul className="space-y-1">
        {NAV.map((item) => {
          const active = item.exact
            ? pathname === item.href
            : pathname.startsWith(item.href);
          const badge = item.href === "/workspace/inbox" && pendingAsks > 0 ? pendingAsks : null;
          return (
            <li key={item.href}>
              <Link
                href={item.href}
                prefetch={false}
                aria-current={active ? "page" : undefined}
                aria-label={`${item.label} — ${item.hint}`}
                className={[
                  "relative flex items-center gap-2.5 rounded-lg px-3 py-2 text-sm transition focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal",
                  active
                    ? "bg-signal/10 font-medium text-foreground before:absolute before:left-0 before:top-1.5 before:bottom-1.5 before:w-0.5 before:rounded-full before:bg-signal"
                    : "text-foreground-muted hover:bg-surface-muted hover:text-foreground",
                ].join(" ")}
              >
                <svg viewBox="0 0 20 20" className="mt-0.5 h-4 w-4 shrink-0 self-start" fill="currentColor" aria-hidden>
                  {ICON[item.icon]}
                </svg>
                <span className="flex flex-1 flex-col leading-tight">
                  <span className="flex items-center gap-1.5">
                    {item.label}
                    {badge != null && (
                      <span
                        className="inline-flex h-4 min-w-4 items-center justify-center rounded-full bg-warning px-1 text-[10px] font-semibold text-white kb-pulse"
                        title={`${badge} decision${badge === 1 ? "" : "s"} waiting on you`}
                      >
                        {badge}
                      </span>
                    )}
                  </span>
                  <span className="text-[11px] text-foreground-muted">{item.hint}</span>
                </span>
              </Link>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}
