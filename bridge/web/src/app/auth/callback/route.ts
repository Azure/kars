// kars Bridge — SSO callback.
//
// GET /auth/callback: completes the OIDC Authorization Code + PKCE flow —
// validates `state` against the stashed flow cookie, exchanges the code for
// tokens, verifies the ID token (signature, issuer, audience, nonce), maps
// the configured role claim through the operator's role map, and mints a
// real signed Bridge session cookie. Any failure is surfaced plainly; it
// never falls through to a silently-granted session.

import { cookies, headers } from "next/headers";
import { NextResponse } from "next/server";
import { oidcConfig } from "@/lib/oidc-config";
import { exchangeCodeForIdentity, rolesFromClaims } from "@/lib/oidc";
import {
  verifyFlowState,
  signSession,
  SESSION_COOKIE,
  OIDC_STATE_COOKIE,
  SESSION_MAX_AGE_SECONDS,
} from "@/lib/session-token";
import { safeReturnTo } from "@/lib/auth-return";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

function errorResponse(code: string, message: string, status = 400): Response {
  return NextResponse.json({ error: { code, message } }, { status });
}

export async function GET(req: Request): Promise<Response> {
  const cfg = oidcConfig();
  if (!cfg) return errorResponse("sso_not_configured", "SSO is not configured on this deployment.", 501);

  const url = new URL(req.url);
  const code = url.searchParams.get("code");
  const state = url.searchParams.get("state");
  const idpError = url.searchParams.get("error");
  if (idpError) {
    return errorResponse("idp_error", `The identity provider returned an error: ${idpError}`);
  }
  if (!code || !state) {
    return errorResponse("missing_code_or_state", "Callback is missing the authorization code or state.");
  }

  const jar = await cookies();
  const flowToken = jar.get(OIDC_STATE_COOKIE)?.value;
  jar.delete(OIDC_STATE_COOKIE);
  if (!flowToken) {
    return errorResponse("missing_flow_cookie", "No pending login found (the flow cookie expired or was already used).");
  }
  const flow = await verifyFlowState(flowToken);
  if (!flow) {
    return errorResponse("invalid_flow_state", "The login flow state is invalid or expired.");
  }
  if (flow.state !== state) {
    return errorResponse("state_mismatch", "OIDC state mismatch — possible CSRF; login aborted.");
  }

  let identity;
  try {
    identity = await exchangeCodeForIdentity(cfg, code, flow.redirectUri, flow.codeVerifier, flow.nonce);
  } catch (e) {
    return errorResponse("token_exchange_failed", e instanceof Error ? e.message : "Token exchange failed.");
  }

  const roles = rolesFromClaims(cfg, identity.claims);
  const session = await signSession({
    sub: identity.sub,
    name: identity.name ?? identity.email ?? identity.sub,
    roles,
  });

  // Redirect to the browser's own origin (Host header), NOT req.url — under a
  // standalone/port-forward deployment req.url carries the pod bind host
  // (0.0.0.0:3000), which is a DIFFERENT origin than the browser's
  // localhost:3000, so the just-set session cookie would not be sent and the
  // user would bounce straight back to /auth/login.
  const h = await headers();
  const proto = h.get("x-forwarded-proto") ?? "http";
  const host = h.get("x-forwarded-host") ?? h.get("host") ?? "localhost:3000";
  const origin = `${proto}://${host}`;
  // Mark the cookie Secure only under real TLS. Over the plain-HTTP
  // kubectl-port-forward path (no ingress, never publicly exposed) a Secure
  // cookie would be dropped by any non-secure-context client, breaking login.
  const secure = proto === "https";

  const auditorOnly =
    roles.includes("auditor") &&
    !roles.some((role) => role === "user" || role === "operator" || role === "admin");
  const defaultDestination = auditorOnly ? "/audit" : "/workspace";
  const destination = roles.length
    ? safeReturnTo(flow.returnTo, defaultDestination)
    : "/auth/no-roles";
  const res = NextResponse.redirect(new URL(destination, origin));
  res.cookies.set(SESSION_COOKIE, session, {
    httpOnly: true,
    secure,
    sameSite: "lax",
    path: "/",
    maxAge: SESSION_MAX_AGE_SECONDS,
  });
  return res;
}
