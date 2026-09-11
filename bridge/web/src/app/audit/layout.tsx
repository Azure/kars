// kars Bridge — the dedicated Auditor surface. A THIRD product surface, separate
// from the employee Workspace and the operator Console: entirely read-only, with
// no policy/skill/fleet write-controls and an auditor identity. An auditor
// confirms the tamper-evident record and verifies receipts independently —
// nothing here can change the platform. Gated on the `auditor` role.

import Link from "next/link";
import { headers } from "next/headers";
import { redirect } from "next/navigation";
import { Icon } from "@/components/icon";
import { ThemeToggle } from "@/components/theme-toggle";
import { environment } from "@/lib/config";
import { currentPrincipal } from "@/lib/session";
import { RoleSwitcher } from "@/components/role-switcher";
import { ssoConfigured } from "@/lib/oidc-config";
import { loginPath, safeReturnTo } from "@/lib/auth-return";

import type { Metadata as _Metadata } from "next";
export const metadata: _Metadata = { title: "Audit" };
export default async function AuditorLayout({ children }: { children: React.ReactNode }) {
  const principal = await currentPrincipal();
  if (ssoConfigured() && principal.roles.length === 0) {
    const requestPath = safeReturnTo(
      (await headers()).get("x-bridge-return-to"),
      "/audit",
    );
    redirect(loginPath(requestPath));
  }
  if (!principal.roles.includes("auditor")) redirect("/workspace");
  const env = environment();
  return (
    <div className="flex min-h-full flex-col">
      <header className="sticky top-0 z-10 border-b border-border bg-surface/90 backdrop-blur">
        <div className="mx-auto flex h-14 max-w-6xl items-center justify-between px-6">
          <Link href="/audit" className="flex items-center gap-2.5 rounded focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal">
            <span className="grid h-7 w-7 place-items-center rounded-md bg-foreground text-background text-sm font-bold">kb</span>
            <span className="font-semibold tracking-tight">
              kars <span className="text-foreground-muted">Auditor</span>
            </span>
          </Link>
          <div className="flex items-center gap-3">
            <span className="hidden items-center gap-1 rounded-full border border-ok/30 bg-ok/[0.08] px-2 py-0.5 text-[11px] font-medium text-ok sm:inline-flex" title="This surface is entirely read-only. Nothing here can change the platform — it only inspects and verifies the tamper-evident record.">
              <Icon name="eye" size={12} /> Read-only
            </span>
            <span className="hidden rounded-full border border-border bg-surface-muted px-2 py-0.5 text-xs font-medium text-foreground-muted sm:inline-flex">
              {env}
            </span>
            <RoleSwitcher
              principal={principal.name}
              primary={principal.primary}
              roles={principal.roles}
              simulated={principal.simulated}
              ssoSignedIn={principal.ssoSignedIn}
              ssoAvailable={ssoConfigured()}
            />
            <ThemeToggle />
          </div>
        </div>
      </header>
      <main id="main-content" className="mx-auto w-full max-w-6xl flex-1 px-6 py-8">{children}</main>
    </div>
  );
}
