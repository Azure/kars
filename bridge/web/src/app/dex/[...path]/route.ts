// kars Bridge — same-origin OIDC IdP proxy (/dex/* → in-cluster Dex).
//
// Why: colleagues reach the Bridge over a single `kubectl port-forward
// svc/kars-bridge-web 3000:3000` — no public ingress, no LoadBalancer. The OIDC
// login flow redirects the *browser* to the IdP's authorization endpoint, so the
// IdP must be reachable at an origin the browser can resolve WITHOUT editing
// /etc/hosts or running a second port-forward for Dex.
//
// By setting Dex's issuer to `http://localhost:3000/dex` and proxying /dex/*
// from this pod to the in-cluster Dex Service, BOTH the browser (authorize +
// login form) AND the server-side token/JWKS/discovery calls flow through the
// one localhost:3000 origin. One port-forward, zero hosts-file hacks.
//
// redirect:"manual" so Dex's 3xx (connector select → login → approval → back to
// /auth/callback) pass straight to the browser. set-cookie is forwarded per
// header (getSetCookie) because Dex carries CSRF/session state in cookies during
// the flow and fetch() otherwise collapses multiples into one comma-joined value.

import { type NextRequest } from "next/server";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

function dexBase(): string {
  return (
    process.env.DEX_UPSTREAM_URL ?? "http://dex.kars-system.svc.cluster.local:5556"
  ).replace(/\/$/, "");
}

// Hop-by-hop headers must not be forwarded verbatim.
const STRIP = new Set([
  "host",
  "connection",
  "keep-alive",
  "proxy-authenticate",
  "proxy-authorization",
  "te",
  "trailer",
  "transfer-encoding",
  "upgrade",
  "content-length",
]);

async function forward(req: NextRequest): Promise<Response> {
  const { pathname, search } = req.nextUrl;
  // pathname already includes the leading /dex segment; Dex is mounted at /dex
  // upstream, so forward it verbatim.
  const target = `${dexBase()}${pathname}${search}`;

  const headers = new Headers();
  req.headers.forEach((value, key) => {
    if (!STRIP.has(key.toLowerCase())) headers.set(key, value);
  });

  const method = req.method.toUpperCase();
  const hasBody = method !== "GET" && method !== "HEAD";

  const init: RequestInit & { duplex?: "half" } = {
    method,
    headers,
    redirect: "manual",
  };
  if (hasBody) {
    init.body = req.body;
    init.duplex = "half";
  }

  let upstream: Response;
  try {
    upstream = await fetch(target, init);
  } catch {
    // OIDC URLs and fetch errors can contain codes or state; log only the event.
    console.error("[bridge/dex] OIDC IdP upstream request failed");
    return new Response(
      JSON.stringify({
        error: {
          code: "bad_gateway",
          message: "OIDC IdP (Dex) unreachable",
        },
      }),
      { status: 502, headers: { "content-type": "application/json" } },
    );
  }

  const respHeaders = new Headers();
  upstream.headers.forEach((value, key) => {
    const k = key.toLowerCase();
    if (k === "set-cookie") return; // handled below, per-cookie
    if (!STRIP.has(k)) respHeaders.set(key, value);
  });

  // Preserve each Set-Cookie header individually — Dex relies on cookies for the
  // login/approval flow, and a comma-joined collapse would corrupt them.
  const setCookies = upstream.headers.getSetCookie?.() ?? [];
  for (const c of setCookies) respHeaders.append("set-cookie", c);

  return new Response(upstream.body, {
    status: upstream.status,
    statusText: upstream.statusText,
    headers: respHeaders,
  });
}

export const GET = forward;
export const POST = forward;
export const PUT = forward;
export const PATCH = forward;
export const DELETE = forward;
export const HEAD = forward;
export const OPTIONS = forward;
