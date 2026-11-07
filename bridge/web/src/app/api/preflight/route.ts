// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { NextRequest } from "next/server";
import { validatePackageAction } from "@/lib/preflight-actions";
import { validateMissionAction } from "@/app/workspace/new/actions";
import { requestRoles } from "@/lib/request-roles";
import { ssoConfigured } from "@/lib/oidc-config";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

function failure(status: number): Response {
  return Response.json({ error: "The readiness check could not be completed." }, {
    status, headers: { "cache-control": "private, no-store" },
  });
}

export async function POST(request: NextRequest): Promise<Response> {
  // Preserve Server Actions' same-origin and 1 MiB request limits on the stable route.
  const origin = request.headers.get("origin");
  try {
    if (!origin || new URL(origin).origin !== origin
      || new URL(origin).host !== request.headers.get("host")) return failure(403);
  } catch {
    return failure(403);
  }
  if (request.headers.get("content-type")?.split(";")[0].trim().toLowerCase() !== "application/json") {
    return failure(415);
  }
  if (ssoConfigured() && (await requestRoles(name => request.cookies.get(name)?.value)).length === 0) {
    return failure(401);
  }
  const reader = request.body?.getReader();
  if (!reader) return failure(400);
  let text = "", size = 0;
  const decoder = new TextDecoder();
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > 1024 * 1024) {
        await reader.cancel();
        return failure(413);
      }
      text += decoder.decode(value, { stream: true });
    }
    text += decoder.decode();
  } catch {
    return failure(400);
  } finally {
    reader.releaseLock();
  }
  let body;
  try { body = JSON.parse(text); } catch { return failure(400); }
  if (!body || typeof body !== "object" || Array.isArray(body)
    || Object.keys(body).some(key => key !== "blueprint" && key !== "envelope")) return failure(400);
  const envelope = body.envelope;
  if (envelope !== undefined && (!envelope || typeof envelope !== "object" || Array.isArray(envelope)
    || (envelope.workload !== undefined && !["team", "mission"].includes(envelope.workload))
    || (envelope.qualification_run !== undefined && typeof envelope.qualification_run !== "boolean"))) {
    return failure(400);
  }
  try {
    const validate = envelope?.workload === "team" ? validatePackageAction : validateMissionAction;
    const result = await validate(body.blueprint, envelope);
    return Response.json(result, { headers: { "cache-control": "private, no-store" } });
  } catch {
    return failure(503);
  }
}
