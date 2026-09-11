// kars Bridge Operator Console — the platform/SRE shell.
//
// Dense, information-first chrome. Unlike the Workspace, Kubernetes context is
// appropriate here — operators reason about namespaces and resources. The
// header carries the substrate scope (namespace + environment) and the switch
// back to the Workspace.

import Link from "next/link";
import { headers } from "next/headers";
import { redirect } from "next/navigation";
import { ConsoleNav } from "@/components/console-nav";
import { SurfaceSwitcher } from "@/components/surface-switcher";
import { RoleSwitcher } from "@/components/role-switcher";
import { ThemeToggle } from "@/components/theme-toggle";
import { defaultNamespace, environment, authWired } from "@/lib/config";
import { currentPrincipal, hasRole } from "@/lib/session";
import { ssoConfigured } from "@/lib/oidc-config";
import { Icon } from "@/components/icon";
import { loginPath, safeReturnTo } from "@/lib/auth-return";

import type { Metadata as _Metadata } from "next";
export const metadata: _Metadata = { title: "Operator Console" };
export default async function ConsoleLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  const principal = await currentPrincipal();
  if (ssoConfigured() && principal.roles.length === 0) {
    const requestPath = safeReturnTo(
      (await headers()).get("x-bridge-return-to"),
      "/console",
    );
    redirect(loginPath(requestPath));
  }
  // Role boundary: the Operator Console is a separate product/permission set.
  // A principal without the `operator` role (admin implies operator) is sent back
  // to their Workspace — the two homes never leak into each other.
  if (!(await hasRole("operator"))) {
    redirect((await hasRole("auditor")) ? "/audit" : "/workspace");
  }
  const ns = defaultNamespace();
  const env = environment();
  const authed = authWired();
  return (
    <div className="flex min-h-full flex-col">
      <header className="sticky top-0 z-10 border-b border-border bg-surface/90 backdrop-blur">
        <div className="mx-auto flex h-14 max-w-7xl items-center justify-between px-6">
          <Link
            href="/console"
            className="flex items-center gap-2.5 rounded focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
          >
            <span className="grid h-7 w-7 place-items-center rounded-md bg-foreground text-background text-sm font-bold">
              kb
            </span>
            <span className="font-semibold tracking-tight">
              kars <span className="text-foreground-muted">Console</span>
            </span>
          </Link>
          <div className="flex items-center gap-3">
            <span
              title={`Namespace: ${ns}`}
              className="hidden items-center gap-1.5 font-mono text-xs text-foreground-muted sm:inline-flex"
            >
              <svg aria-hidden viewBox="0 0 16 16" className="h-3.5 w-3.5" fill="currentColor">
                <path d="M2 4.5A1.5 1.5 0 0 1 3.5 3h3l1.5 1.5h4.5A1.5 1.5 0 0 1 14 6v5.5A1.5 1.5 0 0 1 12.5 13h-9A1.5 1.5 0 0 1 2 11.5v-7Z" />
              </svg>
              {ns}
            </span>
            <span className="hidden rounded-full border border-border bg-surface-muted px-2 py-0.5 text-xs font-medium text-foreground-muted sm:inline-flex">
              {env}
            </span>
            {!authed && (
              <Link
                href="/console/troubleshooting"
                className="hidden items-center gap-1 rounded-full border border-border bg-surface-muted px-2 py-0.5 text-[11px] font-medium text-foreground-muted transition hover:bg-surface hover:text-foreground sm:inline-flex"
                title="Per-user sign-in isn't wired yet. Every read/write goes through the Bridge's Kubernetes ServiceAccount under a least-privilege RBAC role (deploy/rbac.yaml) — the cluster is the real boundary. Click for the full identity disclosure."
              >
                <Icon name="shield" size={12} />
                RBAC-scoped · no SSO
              </Link>
            )}
            <SurfaceSwitcher canAudit={principal.roles.includes("auditor")} />
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

      <div className="mx-auto flex w-full max-w-7xl flex-1 gap-8 px-6 py-8">
        <ConsoleNav />
        <main id="main-content" className="min-w-0 flex-1">
          {children}
        </main>
      </div>
    </div>
  );
}
