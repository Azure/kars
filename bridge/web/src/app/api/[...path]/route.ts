// kars Bridge — runtime same-origin /api/* proxy to the BFF.
//
// Why a route handler (not a next.config rewrite): a standalone build freezes a
// next.config `rewrites` destination at BUILD time, when BRIDGE_BFF_URL is unset,
// pinning every deployment to localhost:8081. This handler runs in the Node
// runtime per request, reads BRIDGE_BFF_URL at RUNTIME, and streams the response
// body (so SSE/EventSource telemetry passes straight through) — the one image
// then works unchanged on kind, AKS, EKS and GKE.
//
// /api/health is a separate static route and takes precedence over this catch-all,
// so the readiness probe is always served locally.

import { type NextRequest } from "next/server";
import { ssoConfigured } from "@/lib/oidc-config";
import { SESSION_COOKIE, verifySession } from "@/lib/session-token";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

function bffBase(): string {
  return (process.env.BRIDGE_BFF_URL ?? "http://localhost:8081").replace(/\/$/, "");
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
  "cookie",
  // Internal-only headers that external requests must never inject.
  "x-teams-internal-secret",
  "x-teams-internal-signature",
]);

async function forward(req: NextRequest): Promise<Response> {
  const { pathname, search } = req.nextUrl;
  const target = `${bffBase()}${pathname}${search}`;

  const headers = new Headers();
  req.headers.forEach((value, key) => {
    if (!STRIP.has(key.toLowerCase()) && key.toLowerCase() !== "x-kars-principal-token") {
      headers.set(key, value);
    }
  });
  if (ssoConfigured()) {
    const token = req.cookies.get(SESSION_COOKIE)?.value;
    if (!token || !(await verifySession(token))) {
      return new Response(
        JSON.stringify({ error: { code: "unauthorized", message: "A signed Bridge session is required." } }),
        { status: 401, headers: { "content-type": "application/json" } },
      );
    }
    headers.set("x-kars-principal-token", token);
  }

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
    // Request URLs and fetch errors can contain credentials; log only the event.
    console.error("[bridge/api] BFF upstream request failed");
    return new Response(
      JSON.stringify({
        error: {
          code: "bad_gateway",
          message: "BFF unreachable",
        },
      }),
      { status: 502, headers: { "content-type": "application/json" } },
    );
  }

  const respHeaders = new Headers();
  upstream.headers.forEach((value, key) => {
    if (!STRIP.has(key.toLowerCase())) respHeaders.set(key, value);
  });

  // Stream the body straight through (works for JSON and SSE alike).
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
