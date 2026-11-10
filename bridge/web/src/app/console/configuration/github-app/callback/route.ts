// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { cookies } from "next/headers";
import { revalidatePath } from "next/cache";
import { canAdminister } from "@/lib/session";
import { SESSION_COOKIE, verifySession } from "@/lib/session-token";
import { convertGithubAppManifest, BffError } from "@/lib/bff";
import {
  githubAppOrigin, verifyGithubAppFlow, GITHUB_APP_FLOW_COOKIE, GITHUB_APP_FLOW_PATH,
} from "@/lib/github-app-registration";

export const dynamic = "force-dynamic";

function result(status: string): Response {
  return new Response(null, { status: 303, headers: {
    Location: `/console/configuration?githubApp=${status}`,
    "Cache-Control": "no-store", "Referrer-Policy": "no-referrer",
  } });
}

export async function GET(request: Request): Promise<Response> {
  // Speculative navigation must not consume GitHub's one-use conversion code.
  if (request.headers.has("rsc") || request.headers.has("next-router-prefetch")
    || /prefetch/i.test(request.headers.get("purpose") ?? request.headers.get("sec-purpose") ?? "")) {
    return new Response(null, { status: 204, headers: { "Cache-Control": "no-store" } });
  }
  if (!await canAdminister()) return result("unauthorized");
  const jar = await cookies();
  const session = jar.get(SESSION_COOKIE)?.value;
  const flow = jar.get(GITHUB_APP_FLOW_COOKIE)?.value;
  if (!session || !await verifySession(session)) return result("session-expired");
  const url = new URL(request.url);
  const state = url.searchParams.get("state");
  let origin: string;
  try {
    origin = githubAppOrigin(request.headers);
  } catch {
    return result("invalid-flow");
  }
  if (!flow || !state || url.searchParams.getAll("state").length !== 1
    || !await verifyGithubAppFlow(flow, session, state, origin)) return result("invalid-flow");
  jar.set(GITHUB_APP_FLOW_COOKIE, "", {
    httpOnly: true, secure: origin.startsWith("https:"), sameSite: "lax", path: GITHUB_APP_FLOW_PATH, maxAge: 0,
  });
  const code = url.searchParams.get("code");
  if (url.searchParams.has("error") || !code) return result("cancelled");
  if (url.searchParams.getAll("code").length !== 1 || !/^[A-Za-z0-9_-]{1,128}$/.test(code)) return result("invalid-flow");
  try {
    await convertGithubAppManifest(code);
  } catch (error) {
    // The code may already have been consumed. Never retry or render upstream details.
    return result(error instanceof BffError && error.code === "conflict"
      ? "configuration-changed" : "conversion-failed");
  }
  revalidatePath("/console/configuration");
  return result("connected");
}
