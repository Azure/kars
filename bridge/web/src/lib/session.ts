// kars Bridge — server-side session/RBAC resolution.
//
// Roles come from, in priority order:
//   1. a REAL signed OIDC session (`bridge-session` cookie) — only present
//      once an operator has configured SSO (BRIDGE_OIDC_ISSUER + friends,
//      see lib/oidc-config.ts) AND a user has completed /auth/login;
//   2. the `bridge-role` cookie — a DEV identity switch so one developer can
//      view the Bridge as each role (admin / operator / user / auditor) and
//      verify the gates hold, WITHOUT standing up an IdP;
//   3. the `BRIDGE_ROLES` env floor (`lib/config.ts::envRoles`);
//   4. default: all roles (single-developer dev convenience).
//
// Whether or not SSO is configured, the REAL authorization boundary remains
// the Bridge's Kubernetes ServiceAccount RBAC (deploy/rbac.yaml) — the roles
// resolved here only drive which UI affordances render, never what the BFF
// is actually allowed to do against the cluster.

import { cookies } from "next/headers";
import {
  type Role,
  envRoles,
  expandRoles,
  parseRoles,
  primaryRole,
  ROLE_META,
} from "./config";
import { ssoConfigured } from "./oidc-config";
import { verifySession, SESSION_COOKIE } from "./session-token";

const COOKIE = "bridge-role";

async function oidcSession() {
  if (!ssoConfigured()) return null;
  const jar = await cookies();
  const token = jar.get(SESSION_COOKIE)?.value;
  if (!token) return null;
  return verifySession(token);
}

/** The active roles for this request (real SSO session → dev cookie → env floor). */
export async function sessionRoles(): Promise<Role[]> {
  const session = await oidcSession();
  if (session) return expandRoles(session.roles);

  // When SSO is configured, there is NO dev-cookie / env-floor fallback. A
  // missing or invalid session means "not signed in" → ZERO roles, and the
  // layout guards (+ the /workspace guard) redirect to /auth/login. This closes
  // the hole where an unauthenticated request would otherwise inherit the
  // `bridge-role` dev cookie or the `BRIDGE_ROLES` floor (which defaults to ALL
  // roles) — i.e. full access with no login. Dev fallbacks apply ONLY when no
  // IdP is configured (single-developer local mode).
  if (ssoConfigured()) return [];

  const jar = await cookies();
  const fromCookie = parseRoles(jar.get(COOKIE)?.value);
  if (fromCookie) return fromCookie;
  return envRoles();
}

export async function hasRole(role: Role): Promise<boolean> {
  return (await sessionRoles()).includes(role);
}

/** True when the principal can take cluster/org-admin actions. */
export async function canAdminister(): Promise<boolean> {
  return (await sessionRoles()).includes("admin");
}

/** The current principal: identity + active roles + a primary badge + whether the
 *  roles are simulated (dev cookie) vs a real signed-in SSO session vs the env floor. */
export async function currentPrincipal(): Promise<{
  name: string;
  roles: Role[];
  primary: Role;
  primaryLabel: string;
  simulated: boolean;
  ssoSignedIn: boolean;
}> {
  const session = await oidcSession();
  if (session) {
    const roles = expandRoles(session.roles);
    const primary = primaryRole(roles);
    return {
      name: session.name,
      roles,
      primary,
      primaryLabel: ROLE_META[primary].label,
      simulated: false,
      ssoSignedIn: true,
    };
  }

  const jar = await cookies();
  const cookieVal = jar.get(COOKIE)?.value;
  // Under SSO, no valid session ⇒ NOT signed in: zero roles, no dev-cookie /
  // env fallback (mirrors sessionRoles). The layout guards redirect to
  // /auth/login. Without SSO (local dev) the dev/env fallback below applies.
  if (ssoConfigured()) {
    return {
      name: "",
      roles: [],
      primary: primaryRole([]),
      primaryLabel: ROLE_META[primaryRole([])].label,
      simulated: false,
      ssoSignedIn: false,
    };
  }
  const simulated = !!parseRoles(cookieVal);
  const roles = simulated ? expandRoles(parseRoles(cookieVal)!) : envRoles();
  const primary = primaryRole(roles);
  const name =
    (simulated ? `${ROLE_META[primary].label.toLowerCase().replace(/\s+/g, "-")}@local` : undefined) ??
    process.env.BRIDGE_OPERATOR ??
    "bridge-operator@local";
  return { name, roles, primary, primaryLabel: ROLE_META[primary].label, simulated, ssoSignedIn: false };
}
