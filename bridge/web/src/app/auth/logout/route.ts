// kars Bridge — SSO logout.
//
// POST /auth/logout: clears the Bridge's own session cookie. Also redirects
// through the IdP's RP-initiated logout (end_session_endpoint) when the IdP
// advertises one, so the IdP-side session ends too — otherwise the user
// would just get silently re-signed-in on their next /auth/login.

import { cookies, headers } from "next/headers";
import { NextResponse } from "next/server";
import { oidcConfig } from "@/lib/oidc-config";
import { endSessionUrl } from "@/lib/oidc";
import { SESSION_COOKIE } from "@/lib/session-token";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

async function requestOrigin(): Promise<string> {
  const h = await headers();
  const proto = h.get("x-forwarded-proto") ?? "http";
  const host = h.get("x-forwarded-host") ?? h.get("host") ?? "localhost:3000";
  return `${proto}://${host}`;
}

export async function POST(): Promise<Response> {
  const jar = await cookies();
  jar.delete(SESSION_COOKIE);

  const cfg = oidcConfig();
  const origin = await requestOrigin();
  // 303 See Other: this handler is reached via POST (the sign-out form), and a
  // 307/308 would make the browser re-POST to the target — hitting a GET-only
  // page (e.g. `/`) with 405 "Method Not Allowed", which looked like a broken
  // "non-existent page". 303 forces the follow-up request to GET.
  if (cfg) {
    const end = await endSessionUrl(cfg, origin).catch(() => null);
    if (end) return NextResponse.redirect(end, 303);
  }
  return NextResponse.redirect(origin, 303);
}
