// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

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
// Under SSO there is no dev fallback. The web and BFF independently verify
// signed roles; the Kubernetes ServiceAccount is a separate aggregate boundary.

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
import { requestRoles } from "./request-roles";

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
  const jar = await cookies();
  return requestRoles((name) => jar.get(name)?.value);
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
  const parsedRoles = parseRoles(cookieVal);
  const simulated = parsedRoles !== null;
  const roles = parsedRoles ?? envRoles();
  const primary = primaryRole(roles);
  const name =
    (simulated ? `${ROLE_META[primary].label.toLowerCase().replace(/\s+/g, "-")}@local` : undefined) ??
    process.env.BRIDGE_OPERATOR ??
    "bridge-operator@local";
  return { name, roles, primary, primaryLabel: ROLE_META[primary].label, simulated, ssoSignedIn: false };
}
