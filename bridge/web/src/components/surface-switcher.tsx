"use client";

// kars Bridge — surface switcher.
//
// The deliberate, infrequent act of crossing between the THREE products: the
// employee Workspace, the operator Console, and the read-only Auditor surface.
// It lives in the header (account area), never auto-linked from content.
// Entitlement-gating lands with auth; today the role flags come from config.

import Link from "next/link";
import { usePathname } from "next/navigation";

type Target = { href: string; label: string };

export function SurfaceSwitcher({
  canOperate = true,
  canAudit = false,
}: {
  canOperate?: boolean;
  canAudit?: boolean;
}) {
  const pathname = usePathname();
  const inConsole = pathname.startsWith("/console");
  const inAudit = pathname.startsWith("/audit");

  const targets: Target[] = [];
  if (inConsole || inAudit) targets.push({ href: "/workspace", label: "Workspace" });
  if (!inConsole && canOperate) targets.push({ href: "/console", label: "Operator Console" });
  if (!inAudit && canAudit) targets.push({ href: "/audit", label: "Auditor" });

  if (targets.length === 0) return null;

  return (
    <div className="flex items-center gap-1.5">
      {targets.map((t) => (
        <Link
          key={t.href}
          href={t.href}
          prefetch={false}
          className="inline-flex items-center gap-1.5 rounded-lg border border-border bg-surface px-2.5 py-1 text-xs font-medium text-foreground-muted transition hover:bg-surface-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
        >
          <svg viewBox="0 0 16 16" className="h-3.5 w-3.5" fill="currentColor" aria-hidden>
            <path d="M5 3 1.5 6.5 5 10V7.5h6V5.5H5V3Zm6 3v2.5H5v2L8.5 13 12 9.5 11 8.5V6h-1Z" />
          </svg>
          {t.label}
        </Link>
      ))}
    </div>
  );
}
