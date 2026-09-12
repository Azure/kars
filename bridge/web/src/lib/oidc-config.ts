// kars Bridge — OIDC SSO configuration (server-only).
//
// Generic, config-only OIDC Authorization Code + PKCE client: point it at any
// standards-compliant IdP (Entra ID, Okta, Auth0, Keycloak, Dex, ...) via env
// vars. HONEST GAP: no IdP is registered in this environment, so
// `oidcConfig()` returns `null` and the whole flow stays inert — /auth/login
// shows an explicit "SSO not configured" state instead of faking a login.
// Wiring a real IdP is a config change only; no code changes are needed.

import type { Role } from "./config";

export interface OidcConfig {
  /** IdP issuer URL. Discovery doc is fetched from `${issuer}/.well-known/openid-configuration`. */
  issuer: string;
  clientId: string;
  clientSecret: string;
  /** Where the IdP redirects back to. Defaults to `{request origin}/auth/callback`. */
  redirectUri: string | null;
  /** OAuth scopes requested. Always includes `openid`. */
  scopes: string[];
  /** The ID-token claim carrying group/role membership (e.g. `groups`, `roles`). */
  roleClaim: string;
  /** Maps a claim value (an IdP group/role name) to a Bridge `Role`. Unmapped
   *  claim values are ignored — a user with no mapped claim gets no roles
   *  (fail-closed), never a default grant. */
  roleMap: Record<string, Role>;
  /** HS256 secret signing the Bridge's own session cookie (not the IdP's keys). */
  sessionSecret: string;
}

/** Parse `BRIDGE_OIDC_ROLE_MAP` — a JSON object of `{"idp-group-name": "role"}`. */
function parseRoleMap(raw: string | undefined): Record<string, Role> {
  if (!raw) return {};
  try {
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    const out: Record<string, Role> = {};
    const valid = new Set(["user", "operator", "auditor", "admin"]);
    for (const [k, v] of Object.entries(parsed)) {
      if (typeof v === "string" && valid.has(v)) out[k] = v as Role;
    }
    return out;
  } catch {
    return {};
  }
}

/** The active OIDC config, or `null` when SSO isn't configured (the honest
 *  default — every field required for a working flow must be present). */
export function oidcConfig(): OidcConfig | null {
  const issuer = process.env.BRIDGE_OIDC_ISSUER?.trim();
  const clientId = process.env.BRIDGE_OIDC_CLIENT_ID?.trim();
  const clientSecret = process.env.BRIDGE_OIDC_CLIENT_SECRET?.trim();
  const sessionSecret = process.env.BRIDGE_SESSION_SECRET?.trim();
  if (!issuer || !clientId || !clientSecret || !sessionSecret) return null;

  const extraScopes = (process.env.BRIDGE_OIDC_SCOPES ?? "profile email")
    .split(/\s+/)
    .map((s) => s.trim())
    .filter(Boolean);

  return {
    issuer: issuer.replace(/\/$/, ""),
    clientId,
    clientSecret,
    redirectUri: process.env.BRIDGE_OIDC_REDIRECT_URI?.trim() || null,
    scopes: Array.from(new Set(["openid", ...extraScopes])),
    roleClaim: process.env.BRIDGE_OIDC_ROLE_CLAIM?.trim() || "roles",
    roleMap: parseRoleMap(process.env.BRIDGE_OIDC_ROLE_MAP),
    sessionSecret,
  };
}

export function ssoConfigured(): boolean {
  return oidcConfig() != null;
}
