"use client";

// kars Bridge Operator Console — primary navigation (platform/SRE surface).
// Dense, resource/policy/audit vocabulary. Kubernetes concepts are fine here.

import Link from "next/link";
import { usePathname } from "next/navigation";

// Grouped by what an operator reaches for: run the fleet, prove/govern it, set it
// up. Cohesive sections make a growing console navigable instead of a flat list.
const GROUPS = [
  {
    title: "Health",
    items: [
      { href: "/console", label: "Fleet Health", exact: true, hint: "Live sandbox status" },
      { href: "/console/fleet", label: "Sandboxes", exact: false, hint: "Every agent pod" },
      { href: "/console/troubleshooting", label: "Troubleshooting", exact: false, hint: "Live diagnostics" },
      { href: "/console/insights", label: "Insights", exact: false, hint: "Fleet efficiency & governance" },
    ],
  },
  {
    title: "Governance",
    items: [
      { href: "/console/policies", label: "Policies", exact: false, hint: "Tools, budgets, egress" },
      { href: "/console/approvals", label: "Approvals", exact: false, hint: "Egress grants" },
      { href: "/console/datapath", label: "Datapath witness", exact: false, hint: "eBPF egress attestation" },
      { href: "/console/evals", label: "Safety evals", exact: false, hint: "Conformance / jailbreak drift" },
      { href: "/console/audit", label: "Auditor view", exact: false, hint: "Receipts & evidence" },
      { href: "/console/sre-actions", label: "SRE Actions", exact: false, hint: "kars-sre remediation proposals" },
    ],
  },
  {
    title: "Setup",
    items: [
      { href: "/console/configuration", label: "Configuration", exact: false, hint: "Cluster & inference provider" },
      { href: "/console/capabilities", label: "Agent capabilities", exact: false, hint: "Skills, team profiles, MCP, credentials" },
      { href: "/console/access", label: "Access & roles", exact: false, hint: "Users, RBAC, permissions" },
    ],
  },
] as const;

export function ConsoleNav() {
  const pathname = usePathname();
  return (
    <nav aria-label="Operator Console" className="hidden w-52 shrink-0 md:block">
      <p className="px-3 pb-2 text-[11px] font-semibold uppercase tracking-wider text-foreground-muted">
        Operator Console
      </p>
      <div className="space-y-4">
        {GROUPS.map((group) => (
          <div key={group.title}>
            <p className="px-3 pb-1 text-[10px] font-semibold uppercase tracking-wider text-foreground-muted/70">
              {group.title}
            </p>
            <ul className="space-y-0.5">
              {group.items.map((item) => {
                const active = item.exact
                  ? pathname === item.href
                  : pathname.startsWith(item.href);
                return (
                  <li key={item.href}>
                    <Link
                      href={item.href}
                      aria-current={active ? "page" : undefined}
                      aria-label={`${item.label} — ${item.hint}`}
                      className={[
                        "relative block rounded-md px-3 py-1.5 text-sm transition focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal",
                        active
                          ? "bg-signal/10 font-medium text-foreground before:absolute before:left-0 before:top-1.5 before:bottom-1.5 before:w-0.5 before:rounded-full before:bg-signal"
                          : "text-foreground-muted hover:bg-surface-muted hover:text-foreground",
                      ].join(" ")}
                    >
                      <span className="block leading-tight">{item.label}</span>
                      <span className="block text-[11px] text-foreground-muted">{item.hint}</span>
                    </Link>
                  </li>
                );
              })}
            </ul>
          </div>
        ))}
      </div>
    </nav>
  );
}
