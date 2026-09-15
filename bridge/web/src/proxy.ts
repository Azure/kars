// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge — server-side RBAC enforcement at the edge.
//
// The Operator Console UI disables admin-only controls, but that is cosmetic: a
// browser can call the BFF directly through the same-origin /api proxy. This
// middleware runs on the WEB SERVER before the request reaches the proxy route
// handler. With SSO, authority comes only from verified signed session roles.
// Dev-cookie/env roles are available only without SSO. The BFF independently
// enforces the same admin requirement, including for direct API clients.
//
// The actual /api/* -> BFF proxying is done at RUNTIME by the catch-all route
// handler app/api/[...path]/route.ts (it reads BRIDGE_BFF_URL per request, so the
// one image works on kind/AKS/EKS/GKE). A next.config `rewrites` would freeze the
// destination at build time.

import { NextResponse, type NextRequest } from "next/server";
import { requestRoles } from "@/lib/request-roles";

// (pathPrefix, methods) tuples that require the `admin` role.
const ADMIN_ONLY: { prefix: string; methods: string[] }[] = [
  { prefix: "/api/operator/inference-budgets", methods: ["PUT", "POST", "PATCH", "DELETE"] },
  { prefix: "/api/operator/retention-policy", methods: ["PUT", "POST", "PATCH", "DELETE"] },
];

export async function proxy(req: NextRequest) {
  const { pathname } = req.nextUrl;
  const method = req.method.toUpperCase();
  const gated = ADMIN_ONLY.find(
    (g) => (pathname === g.prefix || pathname.startsWith(`${g.prefix}/`)) && g.methods.includes(method),
  );
  if (gated && !(await requestRoles((name) => req.cookies.get(name)?.value)).includes("admin")) {
    return NextResponse.json(
      {
        error: {
          code: "forbidden",
          message:
            "Only a cluster or org admin can change this setting.",
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
