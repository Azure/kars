// kars Bridge Workspace — the employee shell.
//
// Consumer-grade chrome: warm, generous spacing, the product identity, the
// current operator identity (honest: no auth session yet), and the surface
// switcher. NO Kubernetes concept ever appears here — no namespace, no pod, no
// CRD. This is the surface that must be flawless for a non-technical task-giver.

import Link from "next/link";
import { headers } from "next/headers";
import { redirect } from "next/navigation";
import { WorkspaceNav } from "@/components/workspace-nav";
import { SurfaceSwitcher } from "@/components/surface-switcher";
import { ThemeToggle } from "@/components/theme-toggle";
import { listApprovals } from "@/lib/bff";
import { operatorIdentity, authWired, defaultNamespace } from "@/lib/config";
import { currentPrincipal } from "@/lib/session";
import { loginPath, safeReturnTo } from "@/lib/auth-return";
import { ssoConfigured } from "@/lib/oidc-config";
import { RoleSwitcher } from "@/components/role-switcher";

import type { Metadata as _Metadata } from "next";
export const metadata: _Metadata = { title: "Workspace" };
export default async function WorkspaceLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  const auth = authWired();
  const principal = await currentPrincipal();
  const who = principal.name || operatorIdentity();
  const requestPath = safeReturnTo(
    (await headers()).get("x-bridge-return-to"),
    "/workspace",
  );
  if (ssoConfigured() && principal.roles.length === 0) {
    redirect(loginPath(requestPath));
  }
  const canUseWorkspace = principal.roles.some((role) =>
    ["user", "operator", "admin"].includes(role),
  );
  if (ssoConfigured() && !canUseWorkspace) {
    redirect(principal.roles.includes("auditor") ? "/audit" : "/auth/no-roles");
  }
  const canOperate = principal.roles.includes("operator");
  let pendingAsks = 0;
  try {
    const approvals = await listApprovals(defaultNamespace());
    pendingAsks = approvals.filter((a) => a.actionable).length;
  } catch {
    pendingAsks = 0;
  }
  return (
    <div className="flex min-h-full flex-col">
      <header className="sticky top-0 z-10 border-b border-border bg-surface/80 backdrop-blur">
        <div className="mx-auto flex h-14 max-w-6xl items-center justify-between px-6">
          <Link
            href="/workspace"
            prefetch={false}
            className="flex items-center gap-2.5 rounded focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
          >
            <span className="grid h-7 w-7 place-items-center rounded-md bg-signal text-signal-fg text-sm font-bold">
              kb
            </span>
            <span className="font-semibold tracking-tight">kars Bridge</span>
          </Link>
          <div className="flex items-center gap-3">
            <span
              className="hidden items-center gap-1.5 text-xs text-foreground-muted sm:inline-flex"
              title={auth ? undefined : "No authenticated session yet — identity is the configured operator."}
            >
              <span className="grid h-5 w-5 place-items-center rounded-full bg-surface-muted text-[10px] font-semibold text-foreground-muted">
                {who.slice(0, 1).toUpperCase()}
              </span>
              {who}
              {!auth && <span className="ml-1 rounded bg-surface-muted px-1.5 py-0.5 text-[10px] font-medium text-foreground-muted/70">dev</span>}
            </span>
            <SurfaceSwitcher canOperate={canOperate} canAudit={principal.roles.includes("auditor")} />
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

      <div className="mx-auto flex w-full max-w-6xl flex-1 gap-8 px-6 py-8">
        <WorkspaceNav pendingAsks={pendingAsks} />
        <main id="main-content" className="min-w-0 flex-1">{children}</main>
      </div>
    </div>
  );
}
