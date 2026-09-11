// kars Bridge — OIDC Authorization Code + PKCE client (server-only).
//
// Standards-compliant against any OIDC-conformant IdP: discovery document,
// PKCE (S256), state + nonce anti-CSRF/replay, JWKS-verified ID token. Uses
// `jose` (audited, dependency-free JWT/JWK library) rather than hand-rolled
// signature verification.

import { createRemoteJWKSet, jwtVerify, type JWTPayload } from "jose";
import { webcrypto } from "node:crypto";
import type { OidcConfig } from "./oidc-config";

interface DiscoveryDoc {
  authorization_endpoint: string;
  token_endpoint: string;
  jwks_uri: string;
  end_session_endpoint?: string;
  issuer: string;
}

// Discovery docs and JWKS are cached per issuer for the process lifetime —
// they change essentially never; refetching on every login would just add
// latency and give the IdP unnecessary load.
const discoveryCache = new Map<string, DiscoveryDoc>();
const jwksCache = new Map<string, ReturnType<typeof createRemoteJWKSet>>();

async function discover(issuer: string): Promise<DiscoveryDoc> {
  const cached = discoveryCache.get(issuer);
  if (cached) return cached;
  const res = await fetch(`${issuer}/.well-known/openid-configuration`);
  if (!res.ok) {
    throw new Error(`OIDC discovery failed for ${issuer}: HTTP ${res.status}`);
  }
  const doc = (await res.json()) as DiscoveryDoc;
  if (!doc.authorization_endpoint || !doc.token_endpoint || !doc.jwks_uri) {
    throw new Error(`OIDC discovery document from ${issuer} is missing required fields`);
  }
  discoveryCache.set(issuer, doc);
  return doc;
}

function jwks(jwksUri: string) {
  let set = jwksCache.get(jwksUri);
  if (!set) {
    set = createRemoteJWKSet(new URL(jwksUri));
    jwksCache.set(jwksUri, set);
  }
  return set;
}

function base64url(bytes: Uint8Array): string {
  return Buffer.from(bytes).toString("base64url");
}

/** Random URL-safe token for `state`/`nonce`/the PKCE code_verifier. */
function randomToken(bytes = 32): string {
  return base64url(webcrypto.getRandomValues(new Uint8Array(bytes)));
}

async function codeChallengeS256(verifier: string): Promise<string> {
  const digest = await webcrypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier));
  return base64url(new Uint8Array(digest));
}

export interface AuthRequest {
  url: string;
  state: string;
  nonce: string;
  codeVerifier: string;
}

/** Build the IdP authorization-endpoint redirect URL + the PKCE/anti-replay
 *  values the caller must stash (in a short-lived signed cookie) to validate
 *  the callback. */
export async function buildAuthorizationRequest(
  cfg: OidcConfig,
  redirectUri: string,
): Promise<AuthRequest> {
  const doc = await discover(cfg.issuer);
  const state = randomToken(16);
  const nonce = randomToken(16);
  const codeVerifier = randomToken(32);
  const codeChallenge = await codeChallengeS256(codeVerifier);

  const params = new URLSearchParams({
    response_type: "code",
    client_id: cfg.clientId,
    redirect_uri: redirectUri,
    scope: cfg.scopes.join(" "),
    state,
    nonce,
    code_challenge: codeChallenge,
    code_challenge_method: "S256",
  });

  return { url: `${doc.authorization_endpoint}?${params.toString()}`, state, nonce, codeVerifier };
}

export interface OidcIdentity {
  sub: string;
  name: string | null;
  email: string | null;
  claims: JWTPayload;
}

/** Exchange the authorization code for tokens and verify the ID token's
 *  signature (against the IdP's live JWKS), issuer, audience, expiry, and
 *  nonce. Throws on any verification failure — callers must not treat a
 *  thrown error as "signed in with no claims". */
export async function exchangeCodeForIdentity(
  cfg: OidcConfig,
  code: string,
  redirectUri: string,
  codeVerifier: string,
  expectedNonce: string,
): Promise<OidcIdentity> {
  const doc = await discover(cfg.issuer);

  const body = new URLSearchParams({
    grant_type: "authorization_code",
    code,
    redirect_uri: redirectUri,
    client_id: cfg.clientId,
    client_secret: cfg.clientSecret,
    code_verifier: codeVerifier,
  });
  const res = await fetch(doc.token_endpoint, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body,
  });
  if (!res.ok) {
    throw new Error(`OIDC token exchange failed: HTTP ${res.status} ${await res.text().catch(() => "")}`);
  }
  const tokens = (await res.json()) as { id_token?: string };
  if (!tokens.id_token) {
    throw new Error("OIDC token response carried no id_token");
  }

  const { payload } = await jwtVerify(tokens.id_token, jwks(doc.jwks_uri), {
    issuer: doc.issuer,
    audience: cfg.clientId,
  });

  if (payload.nonce !== expectedNonce) {
    throw new Error("OIDC id_token nonce mismatch — possible replay");
  }
  if (!payload.sub) {
    throw new Error("OIDC id_token carried no sub claim");
  }

  return {
    sub: payload.sub,
    name: (payload.name as string | undefined) ?? (payload.preferred_username as string | undefined) ?? null,
    email: (payload.email as string | undefined) ?? null,
    claims: payload,
  };
}

/** Map the configured role claim (string or string[]) through `roleMap`.
 *  Fail-closed: an unrecognized or absent claim yields no roles, never a
 *  default grant — the operator must explicitly map IdP groups to roles. */
export function rolesFromClaims(cfg: OidcConfig, claims: JWTPayload): import("./config").Role[] {
  const raw = claims[cfg.roleClaim];
  const values = Array.isArray(raw) ? raw : typeof raw === "string" ? [raw] : [];
  const roles = new Set<import("./config").Role>();
  for (const v of values) {
    const mapped = cfg.roleMap[String(v)];
    if (mapped) roles.add(mapped);
  }
  return Array.from(roles);
}

/** End-session (RP-initiated logout) URL, when the IdP advertises one. */
export async function endSessionUrl(cfg: OidcConfig, postLogoutRedirectUri: string): Promise<string | null> {
  const doc = await discover(cfg.issuer);
  if (!doc.end_session_endpoint) return null;
  const params = new URLSearchParams({
    client_id: cfg.clientId,
    post_logout_redirect_uri: postLogoutRedirectUri,
  });
  return `${doc.end_session_endpoint}?${params.toString()}`;
}
