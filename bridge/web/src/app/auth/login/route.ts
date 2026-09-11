// kars Bridge — SSO login entry point.
//
// GET /auth/login: when SSO is configured, starts a real OIDC Authorization
// Code + PKCE flow (redirects to the IdP). When it isn't configured (the
// honest default in this environment — no IdP registered), returns a plain
// explanation instead of a broken or faked login screen.

import { cookies, headers } from "next/headers";
import { NextResponse } from "next/server";
import { oidcConfig } from "@/lib/oidc-config";
import { buildAuthorizationRequest } from "@/lib/oidc";
import { signFlowState, OIDC_STATE_COOKIE } from "@/lib/session-token";
import { safeReturnTo } from "@/lib/auth-return";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

async function requestOrigin(): Promise<string> {
  const h = await headers();
  const proto = h.get("x-forwarded-proto") ?? "http";
  const host = h.get("x-forwarded-host") ?? h.get("host") ?? "localhost:3000";
  return `${proto}://${host}`;
}

export async function GET(req: Request): Promise<Response> {
  const cfg = oidcConfig();
  if (!cfg) {
    return NextResponse.json(
      {
        error: {
          code: "sso_not_configured",
          message:
            "SSO is not configured on this Bridge deployment. Set BRIDGE_OIDC_ISSUER, " +
            "BRIDGE_OIDC_CLIENT_ID, BRIDGE_OIDC_CLIENT_SECRET, and BRIDGE_SESSION_SECRET " +
            "to connect a real OIDC provider (Entra ID, Okta, Auth0, Keycloak, ...). " +
            "Until then, use the role switcher for a simulated dev identity.",
        },
      },
      { status: 501 },
    );
  }

  const redirectUri = cfg.redirectUri ?? `${await requestOrigin()}/auth/callback`;
  const returnTo = safeReturnTo(new URL(req.url).searchParams.get("returnTo"));
  const authReq = await buildAuthorizationRequest(cfg, redirectUri);
  const flowToken = await signFlowState({
    state: authReq.state,
    nonce: authReq.nonce,
    codeVerifier: authReq.codeVerifier,
    redirectUri,
    returnTo,
  });

  const jar = await cookies();
  const h = await headers();
  const secure = (h.get("x-forwarded-proto") ?? "http") === "https";
  jar.set(OIDC_STATE_COOKIE, flowToken, {
    httpOnly: true,
    secure,
    sameSite: "lax",
    path: "/",
    maxAge: 10 * 60,
  });

  return NextResponse.redirect(authReq.url);
}
