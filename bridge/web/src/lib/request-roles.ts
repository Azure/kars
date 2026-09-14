// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { type Role, envRoles, expandRoles, parseRoles } from "./config";
import { ssoConfigured } from "./oidc-config";
import { SESSION_COOKIE, verifySession } from "./session-token";

/** Shared by server components and the request proxy; dev roles never grant SSO access. */
export async function requestRoles(
  cookie: (name: string) => string | undefined,
): Promise<Role[]> {
  if (ssoConfigured()) {
    const token = cookie(SESSION_COOKIE);
    const session = token ? await verifySession(token) : null;
    return session ? expandRoles(session.roles) : [];
  }
  return parseRoles(cookie("bridge-role")) ?? envRoles();
}
