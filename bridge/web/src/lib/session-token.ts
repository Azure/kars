// kars Bridge — signed session cookie (server-only).
//
// A real OIDC login mints one of these: a compact, HS256-signed JWT (never
// the IdP's own tokens — those aren't the Bridge's to hold onto) carrying just
// what the Bridge needs: subject, display name, and the roles resolved from
// the IdP's group/role claim at login time. Roles are snapshotted at login,
// not re-derived per-request — matches standard session semantics (a role
// change at the IdP takes effect on next login, not instantly).

import { SignJWT, jwtVerify } from "jose";
import type { Role } from "./config";
import { oidcConfig } from "./oidc-config";

export interface BridgeSession {
  sub: string;
  name: string;
  roles: Role[];
}

const ALG = "HS256";
const MAX_AGE_SECONDS = 60 * 60 * 8; // 8h — a real login session, not indefinite.

function secretKey(secret: string): Uint8Array {
  return new TextEncoder().encode(secret);
}

/** Sign a session for a just-authenticated user. */
export async function signSession(session: BridgeSession): Promise<string> {
  const cfg = oidcConfig();
  if (!cfg) throw new Error("cannot sign a session: SSO is not configured");
  return new SignJWT({ name: session.name, roles: session.roles })
    .setProtectedHeader({ alg: ALG })
    .setSubject(session.sub)
    .setIssuedAt()
    .setExpirationTime(`${MAX_AGE_SECONDS}s`)
    .sign(secretKey(cfg.sessionSecret));
}

/** Verify + decode a session cookie. Returns `null` on any failure (expired,
 *  tampered, wrong key, or SSO no longer configured) — callers must fall back
 *  to the existing dev-role-switch/env-floor path, never treat a failure as
 *  "signed in with no roles". */
export async function verifySession(token: string): Promise<BridgeSession | null> {
  const cfg = oidcConfig();
  if (!cfg) return null;
  try {
    const { payload } = await jwtVerify(token, secretKey(cfg.sessionSecret), { algorithms: [ALG] });
    if (!payload.sub) return null;
    const roles = Array.isArray(payload.roles) ? (payload.roles as Role[]) : [];
    return { sub: payload.sub, name: (payload.name as string | undefined) ?? payload.sub, roles };
  } catch {
    return null;
  }
}

export const SESSION_COOKIE = "bridge-session";
export const OIDC_STATE_COOKIE = "bridge-oidc-state";
export const SESSION_MAX_AGE_SECONDS = MAX_AGE_SECONDS;

/** The transient PKCE/anti-replay values stashed between /auth/login and
 *  /auth/callback, signed the same way as a session but with a short TTL. */
export interface OidcFlowState {
  state: string;
  nonce: string;
  codeVerifier: string;
  redirectUri: string;
  returnTo?: string;
}

const FLOW_MAX_AGE_SECONDS = 10 * 60; // 10min — long enough for a real login, short enough to bound replay.

export async function signFlowState(flow: OidcFlowState): Promise<string> {
  const cfg = oidcConfig();
  if (!cfg) throw new Error("cannot sign OIDC flow state: SSO is not configured");
  return new SignJWT({ ...flow })
    .setProtectedHeader({ alg: ALG })
    .setIssuedAt()
    .setExpirationTime(`${FLOW_MAX_AGE_SECONDS}s`)
    .sign(secretKey(cfg.sessionSecret));
}

export async function verifyFlowState(token: string): Promise<OidcFlowState | null> {
  const cfg = oidcConfig();
  if (!cfg) return null;
  try {
    const { payload } = await jwtVerify(token, secretKey(cfg.sessionSecret), { algorithms: [ALG] });
    const { state, nonce, codeVerifier, redirectUri, returnTo } =
      payload as Partial<OidcFlowState>;
    if (!state || !nonce || !codeVerifier || !redirectUri) return null;
    return {
      state,
      nonce,
      codeVerifier,
      redirectUri,
      returnTo: typeof returnTo === "string" ? returnTo : undefined,
    };
  } catch {
    return null;
  }
}
