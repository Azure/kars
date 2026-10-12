// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge Operator Console — Access & roles. The multi-user surface: the four
// differentiated permission sets, current principal and deployment identity mode.

import { PageHeader, Section } from "@/components/ui";
import { ALL_ROLES, ROLE_META, type Role } from "@/lib/config";
import { currentPrincipal } from "@/lib/session";
import { ssoConfigured } from "@/lib/oidc-config";
import { Icon } from "@/components/icon";

export const dynamic = "force-dynamic";

// The capability matrix — the concrete surfaces/actions each role holds. Ordered
// by privilege; a ✓ means the role can do it (with implication: admin ⊇ operator
// ⊇ user; admin ⊇ auditor).
const CAPABILITIES: { area: string; cap: string; roles: Role[] }[] = [
  { area: "Workspace", cap: "Start & review missions and teams", roles: ["user", "operator", "admin"] },
  { area: "Workspace", cap: "Connect GitHub & channels", roles: ["user", "operator", "admin"] },
  { area: "Workspace", cap: "Approve / request changes on deliverables", roles: ["user", "operator", "admin"] },
  { area: "Auditor", cap: "Read the audit log", roles: ["auditor", "admin"] },
  { area: "Evidence", cap: "Read and verify accessible receipts", roles: ["user", "auditor", "operator", "admin"] },
  { area: "Console", cap: "Manage tool / inference policies, skills, MCP", roles: ["operator", "admin"] },
  { area: "Console", cap: "Approve egress grants, run safety evals", roles: ["operator", "admin"] },
  { area: "Console", cap: "View fleet, sandboxes, insights", roles: ["operator", "admin"] },
  { area: "Console", cap: "Cluster & provider configuration", roles: ["operator", "admin"] },
  { area: "Admin", cap: "Manage inference budgets & retention policy", roles: ["admin"] },
];

function Check({ on }: { on: boolean }) {
  return on ? (
    <span className="text-signal" aria-label="allowed">✓</span>
  ) : (
    <span className="text-foreground-muted/40" aria-label="not allowed">·</span>
  );
}

export default async function AccessPage() {
  const principal = await currentPrincipal();
  const sso = ssoConfigured();
  return (
    <div className="space-y-6">
      <PageHeader
        title="Access & roles"
        lead="kars Bridge has four permission sets. Signed-in roles are checked by the web tier and independently by the API; resource policies and Kubernetes RBAC can further constrain an operation. Admin includes operator and auditor permissions; operator includes workspace user permissions."
      />

      <Section title="You" subtitle="The principal this session is acting as.">
        <div className="flex flex-wrap items-center gap-3 rounded-lg border border-border bg-surface-muted/40 p-4">
          <span className="grid h-9 w-9 place-items-center rounded-full bg-surface text-lg" aria-hidden>
            <Icon name={ROLE_META[principal.primary].glyph} size={18} />
          </span>
          <div>
            <p className="font-mono text-sm">{principal.name}</p>
            <p className="text-xs text-foreground-muted">
              Acting as <span className="font-medium text-foreground">{principal.primaryLabel}</span> · holds{" "}
              {principal.roles.map((r) => ROLE_META[r].label).join(", ")}
            </p>
          </div>
          <span
            className={`ml-auto rounded-full border px-2.5 py-1 text-[11px] font-medium ${
              principal.ssoSignedIn
                ? "border-ok/30 bg-ok/[0.08] text-ok"
                : "border-amber-500/30 bg-amber-500/[0.08] text-amber-600"
            }`}
          >
            {principal.ssoSignedIn
              ? "Real SSO session"
              : principal.simulated
                ? "Simulated role (dev — no SSO session)"
                : "Roles from BRIDGE_ROLES env"}
          </span>
        </div>
        <p className="mt-2 text-xs text-foreground-muted">
          {principal.ssoSignedIn ? (
            <>
              Your roles came from your identity provider&rsquo;s group claims at login, mapped via{" "}
              <code className="rounded bg-surface-muted px-1">BRIDGE_OIDC_ROLE_MAP</code>. The API independently
              verifies your signed Bridge session and checks the required role for each route. The
              Bridge&rsquo;s Kubernetes ServiceAccount RBAC is a separate cluster-permission ceiling;
              visible controls cannot grant permissions beyond either boundary.
            </>
          ) : (
            <>
              This session has no verified per-user sign-in. Local development roles come from
              <code className="rounded bg-surface-muted px-1">BRIDGE_ROLES</code>; a simulated view is not
              proof of an independently authenticated role. Configure SSO and the shared session secret
              on both web and API for per-user enforcement. Kubernetes ServiceAccount RBAC remains
              a separate cluster-permission ceiling.
            </>
          )}
        </p>
      </Section>

      <Section
        title="Single sign-on"
        subtitle="Connect a real OIDC identity provider (Entra ID, Okta, Auth0, Keycloak, ...) — config only, no code changes."
      >
        <div className={`rounded-lg border p-4 ${sso ? "border-ok/30 bg-ok/[0.04]" : "border-dashed border-border bg-surface-muted/30"}`}>
          <p className="flex items-center gap-2 text-sm font-medium">
            {sso ? (
              <>
                <span className="text-ok">✓</span> SSO configured
              </>
            ) : (
              <span className="text-foreground-muted">SSO not configured</span>
            )}
          </p>
          <p className="mt-1 text-xs text-foreground-muted">
            {sso ? (
              <>
                An OIDC provider is connected. Users sign in at{" "}
                <code className="rounded bg-surface-muted px-1">/auth/login</code> — a real Authorization Code +
                PKCE flow, ID-token signature/issuer/audience/nonce verified against the provider&rsquo;s live
                JWKS, roles mapped from the configured group claim, and a signed Bridge session issued. No
                credential or IdP token the Bridge holds ever reaches the browser.
              </>
            ) : (
              <>
                Set <code className="rounded bg-surface-muted px-1">BRIDGE_OIDC_ISSUER</code>,{" "}
                <code className="rounded bg-surface-muted px-1">BRIDGE_OIDC_CLIENT_ID</code>,{" "}
                <code className="rounded bg-surface-muted px-1">BRIDGE_OIDC_CLIENT_SECRET</code> (from a K8s
                Secret, never inline), and <code className="rounded bg-surface-muted px-1">BRIDGE_SESSION_SECRET</code>{" "}
                on the web deployment to connect a real IdP. Optionally set{" "}
                <code className="rounded bg-surface-muted px-1">BRIDGE_OIDC_ROLE_CLAIM</code> (default{" "}
                <code className="rounded bg-surface-muted px-1">roles</code>) and{" "}
                <code className="rounded bg-surface-muted px-1">BRIDGE_OIDC_ROLE_MAP</code> (JSON, e.g.{" "}
                <code className="rounded bg-surface-muted px-1">{`{"kars-admins":"admin"}`}</code>) to map your
                IdP&rsquo;s groups to Bridge roles — unmapped groups get zero roles (fail-closed).
              </>
            )}
          </p>
        </div>
      </Section>

      <Section title="The four roles" subtitle="Separate products, separate permission sets.">
        <p className="mb-3 text-sm text-foreground-muted">
          Role membership is managed by your identity provider and the deployment&rsquo;s{" "}
          <code>BRIDGE_OIDC_ROLE_MAP</code>, not on this page. Sign in again after changes to refresh
          your session roles.
        </p>
        <div className="grid gap-3 sm:grid-cols-2">
          {ALL_ROLES.map((r) => {
            const m = ROLE_META[r];
            const isYou = principal.primary === r;
            return (
              <div
                key={r}
                className={`rounded-lg border p-4 ${isYou ? "border-signal/40 bg-signal/[0.04]" : "border-border bg-surface"}`}
              >
                <div className="flex items-center gap-2">
                  <span aria-hidden className="text-lg"><Icon name={m.glyph} size={18} /></span>
                  <span className="text-sm font-semibold">{m.label}</span>
                  {isYou && (
                    <span className="rounded-full border border-signal/40 bg-signal/10 px-1.5 py-0.5 text-[10px] font-medium text-signal">
                      you
                    </span>
                  )}
                </div>
                <p className="mt-1 text-xs text-foreground-muted">{m.blurb}</p>
              </div>
            );
          })}
        </div>
      </Section>

      <Section title="Capability matrix" subtitle="Role-level access; individual operations also require their resource and policy checks.">
        <div className="overflow-x-auto rounded-lg border border-border">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-border bg-surface-muted/40 text-left text-xs text-foreground-muted">
                <th scope="col" className="px-4 py-2 font-medium">Area</th>
                <th scope="col" className="px-3 py-2 font-medium">Capability</th>
                {ALL_ROLES.map((r) => (
                  <th key={r} scope="col" className="px-3 py-2 text-center font-medium" title={ROLE_META[r].label} aria-label={ROLE_META[r].label}>
                    <Icon name={ROLE_META[r].glyph} size={14} className="inline" />
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {CAPABILITIES.map((c, i) => (
                <tr key={i} className="border-b border-border last:border-0">
                  <td className="px-4 py-2 text-xs text-foreground-muted">{c.area}</td>
                  <td className="px-3 py-2">{c.cap}</td>
                  {ALL_ROLES.map((r) => (
                    <td key={r} className="px-3 py-2 text-center">
                      <Check on={c.roles.includes(r)} />
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
        <p className="mt-2 flex flex-wrap gap-x-4 gap-y-1 text-[11px] text-foreground-muted">
          {ALL_ROLES.map((r) => (
            <span key={r} className="inline-flex items-center gap-1">
              <Icon name={ROLE_META[r].glyph} size={12} /> {ROLE_META[r].label}
            </span>
          ))}
        </p>
      </Section>
    </div>
  );
}
