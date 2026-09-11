// kars Bridge web — runtime configuration.
//
// All values are read server-side. The browser never receives cluster
// credentials or signing keys; it only ever talks to the BFF through the
// Next.js server (see lib/bff.ts).

import { ssoConfigured } from "./oidc-config";
import type { IconName } from "@/components/icon";

/** Base URL of the kars Bridge BFF, reachable from the Next.js server. */
export function bffBaseUrl(): string {
  return process.env.BRIDGE_BFF_URL ?? "http://localhost:8081";
}

/** Default namespace the UI scopes task views to. */
export function defaultNamespace(): string {
  return process.env.BRIDGE_DEFAULT_NAMESPACE ?? "kars-system";
}

/** Deployment environment label shown in the header (local/staging/prod). */
export function environment(): string {
  return process.env.BRIDGE_ENV ?? "local";
}

/**
 * Identity recorded as the decider on an approval.
 *
 * HONEST GAP: the Bridge does not yet have an authenticated session, so a
 * decision made through the UI is attributed to this configured operator
 * identity rather than a logged-in user. The UI surfaces this plainly next to
 * the decision controls — we never imply a per-user identity we cannot prove.
 * Wiring real auth (and binding the decider to it) is a follow-up.
 */
export function operatorIdentity(): string {
  return process.env.BRIDGE_OPERATOR ?? "bridge-operator@local";
}

/** Whether real per-user auth is wired (drives the honest decider caveat).
 *  True once an operator sets BRIDGE_AUTH_WIRED=true directly, OR
 *  automatically once real OIDC SSO is configured (lib/oidc-config.ts) —
 *  configuring a working IdP connection IS wiring real auth, no separate
 *  flag to remember to flip. */
export function authWired(): boolean {
  if (process.env.BRIDGE_AUTH_WIRED === "true") return true;
  return ssoConfigured();
}

/** Optional deep-link to a Headlamp (or any K8s dashboard) instance for this
 *  cluster. When set, the operator console surfaces a launch link; when unset,
 *  it shows how to enable it rather than a dead button. */
export function headlampUrl(): string | null {
  const u = process.env.BRIDGE_HEADLAMP_URL;
  return u && u.trim().length > 0 ? u : null;
}

/**
 * The FOUR role sets that partition kars Bridge, in privilege order:
 * - `user`     — the employee Workspace: start missions/teams, review, connections.
 * - `auditor`  — read-only Auditor surface: receipts, evidence. Changes nothing.
 * - `operator` — the Operator Console: providers, policies, skills, fleet, evals,
 *                egress approvals. Manages governance for everyone's work.
 * - `admin`    — cluster/org administrator: everything an operator can do PLUS the
 *                highest-privilege actions — raising cluster/workspace inference
 *                budgets, cluster/provider configuration, and the roles surface.
 * They are SEPARATE permission sets and (with SSO) map to distinct groups. `admin`
 * implies `operator`, `auditor`, and `user`; `operator` implies `user`.
 */
export type Role = "user" | "operator" | "auditor" | "admin";

export const ALL_ROLES: Role[] = ["user", "auditor", "operator", "admin"];

/** Human labels + one-line capability summaries for the roles surface. */
export const ROLE_META: Record<Role, { label: string; blurb: string; glyph: IconName }> = {
  user: {
    label: "Workspace user",
    blurb: "Start & review missions and teams, connect GitHub/channels. The employee surface.",
    glyph: "person",
  },
  auditor: {
    label: "Auditor",
    blurb: "Read-only receipts, evidence, and the tamper-evident log. Changes nothing.",
    glyph: "search",
  },
  operator: {
    label: "Operator",
    blurb: "Govern the fleet: tool/inference policies, skills, MCP, evals, egress approvals.",
    glyph: "wrench",
  },
  admin: {
    label: "Admin",
    blurb: "Cluster/org administration: raise inference budgets, provider config, manage roles.",
    glyph: "crown",
  },
};

/** Expand a set of granted roles to include everything they imply (admin ⊇
 *  operator ⊇ user; admin ⊇ auditor). Auditor is a separate read-only persona
 *  and does not imply employee Workspace access. */
export function expandRoles(granted: Role[]): Role[] {
  const set = new Set<Role>(granted);
  if (set.has("admin")) {
    set.add("operator");
    set.add("auditor");
  }
  if (set.has("operator")) set.add("user");
  return ALL_ROLES.filter((r) => set.has(r));
}

/** Parse a comma-separated role string into a valid, implication-expanded set. */
export function parseRoles(raw: string | undefined | null): Role[] | null {
  if (!raw) return null;
  const roles = raw
    .split(",")
    .map((r) => r.trim().toLowerCase())
    .filter((r): r is Role => (ALL_ROLES as string[]).includes(r));
  return roles.length ? expandRoles(roles) : null;
}

/** The highest role held, for a single primary badge. */
export function primaryRole(roles: Role[]): Role {
  if (roles.includes("admin")) return "admin";
  if (roles.includes("operator")) return "operator";
  if (roles.includes("auditor")) return "auditor";
  return "user";
}

/**
 * The current principal's roles from the STATIC env (`BRIDGE_ROLES`), expanded by
 * implication; defaults to ALL in dev so a single developer sees everything. The
 * cookie-aware, switchable session lives in `lib/session.ts` (server-only) and
 * layers on top of this — use that in server components; this is the env floor.
 */
export function envRoles(): Role[] {
  return parseRoles(process.env.BRIDGE_ROLES) ?? ["admin", "operator", "auditor", "user"];
}

/** @deprecated in server components use `sessionRoles()` from lib/session.ts. */
export function sessionRoles(): Role[] {
  return envRoles();
}

export function hasRole(role: Role): boolean {
  return sessionRoles().includes(role);
}
