// kars Bridge — server-side RBAC enforcement at the edge.
//
// The Operator Console UI disables admin-only controls, but that is cosmetic: a
// browser can call the BFF directly through the same-origin /api proxy. This
// middleware runs on the WEB SERVER before the request reaches the proxy route
// handler, so it enforces admin-only mutations for real — the role comes from the
// httpOnly `bridge-role` cookie the browser cannot forge, falling back to the
// BRIDGE_ROLES env floor, exactly like lib/session.ts.
//
// The actual /api/* -> BFF proxying is done at RUNTIME by the catch-all route
// handler app/api/[...path]/route.ts (it reads BRIDGE_BFF_URL per request, so the
// one image works on kind/AKS/EKS/GKE). A next.config `rewrites` would freeze the
// destination at build time.

import { NextResponse, type NextRequest } from "next/server";
import { parseRoles, envRoles } from "@/lib/config";

// (pathPrefix, methods) tuples that require the `admin` role.
const ADMIN_ONLY: { prefix: string; methods: string[] }[] = [
  { prefix: "/api/operator/inference-budgets", methods: ["PUT", "POST", "DELETE"] },
  { prefix: "/api/operator/retention-policy", methods: ["PUT", "POST", "DELETE"] },
];

function isAdmin(req: NextRequest): boolean {
  const cookie = req.cookies.get("bridge-role")?.value;
  const roles = parseRoles(cookie) ?? envRoles();
  return roles.includes("admin");
}

export function proxy(req: NextRequest) {
  const { pathname } = req.nextUrl;
  const method = req.method.toUpperCase();
  const gated = ADMIN_ONLY.find(
    (g) => pathname.startsWith(g.prefix) && g.methods.includes(method),
  );
  if (gated && !isAdmin(req)) {
    return NextResponse.json(
      {
        error: {
          code: "forbidden",
          message:
            "Only a cluster or org admin can change this setting. Switch to the Admin role (or ask an admin).",
        },
      },
      { status: 403 },
    );
  }
  const requestHeaders = new Headers(req.headers);
  requestHeaders.set(
    "x-bridge-return-to",
    `${req.nextUrl.pathname}${req.nextUrl.search}`,
  );
  return NextResponse.next({ request: { headers: requestHeaders } });
}

export const config = {
  matcher: [
    "/workspace/:path*",
    "/console/:path*",
    "/audit/:path*",
    "/api/operator/inference-budgets/:path*",
    "/api/operator/retention-policy",
  ],
};
