// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use server";

// kars Bridge Operator Console — GitHub App self-service setup. Replaces the
// manual `kubectl create secret` step with a real form: verifies the App
// credentials against GitHub's own API before storing them, so a typo'd key
// fails loudly here instead of silently later.

import { revalidatePath } from "next/cache";
import { cookies, headers } from "next/headers";
import { putGithubApp, deleteGithubApp, getGithubApp, BffError } from "@/lib/bff";
import { canAdminister, hasRole } from "@/lib/session";
import { SESSION_COOKIE, verifySession } from "@/lib/session-token";
import {
  createGithubAppFlow, githubAppManifest, githubAppOrigin, readGithubAppPem,
  GITHUB_APP_FLOW_COOKIE, GITHUB_APP_FLOW_PATH, GITHUB_APP_FLOW_SECONDS, GithubAppInputError,
} from "@/lib/github-app-registration";

export interface GithubAppState {
  error: string | null;
  ok: string | null;
}

export async function putGithubAppAction(_prev: GithubAppState, form: FormData): Promise<GithubAppState> {
  if (!await hasRole("operator")) return { error: "Operator access is required.", ok: null };
  try {
    githubAppOrigin(await headers(), true);
  } catch {
    return { error: "Open Configuration over HTTPS or a localhost connection before uploading a key.", ok: null };
  }
  let credentials: Awaited<ReturnType<typeof readGithubAppPem>>;
  try {
    credentials = await readGithubAppPem(form);
  } catch (error) {
    return { error: error instanceof GithubAppInputError ? error.message : "Could not read the private-key file.", ok: null };
  }
  try {
    const existing = await getGithubApp();
    if (existing.configured) return { error: "An App is already configured. Disconnect it explicitly before replacing it.", ok: null };
    const r = await putGithubApp(credentials);
    revalidatePath("/console/configuration");
    return { error: null, ok: `Verified against GitHub${r.name ? ` — ${r.name}` : ""}. ${r.note}` };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "Setup failed. No private-key contents are displayed.", ok: null };
  }
}

export interface GithubAppRegistrationState {
  error: string | null;
  registration: { url: string; manifest: string } | null;
}

export async function startGithubAppRegistrationAction(
  _prev: GithubAppRegistrationState, form: FormData,
): Promise<GithubAppRegistrationState> {
  if (!await canAdminister()) return { error: "Administrator access is required.", registration: null };
  const jar = await cookies();
  const sessionToken = jar.get(SESSION_COOKIE)?.value;
  if (!sessionToken || !await verifySession(sessionToken)) {
    return { error: "Guided registration requires a signed-in SSO session. Sign in first, or upload an existing App’s PEM file.", registration: null };
  }
  let origin: string;
  try {
    origin = githubAppOrigin(await headers(), true);
  } catch {
    return { error: "Guided registration requires HTTPS or a localhost connection.", registration: null };
  }
  let registration: ReturnType<typeof githubAppManifest>;
  try {
    registration = githubAppManifest(form, origin);
  } catch (error) {
    return { error: error instanceof GithubAppInputError ? error.message : "Check the registration choices.", registration: null };
  }
  try {
    const app = await getGithubApp();
    if (app.configured) return { error: "An App is already configured. Disconnect it explicitly before registering a replacement.", registration: null };
    if (!app.storage_ready) return { error: "Enroll GitHub App credential storage before starting registration.", registration: null };
    const flow = await createGithubAppFlow(sessionToken, origin);
    jar.set(GITHUB_APP_FLOW_COOKIE, flow.token, {
      httpOnly: true, secure: origin.startsWith("https:"), sameSite: "lax",
      path: GITHUB_APP_FLOW_PATH, maxAge: GITHUB_APP_FLOW_SECONDS,
    });
    return { error: null, registration: {
      url: `${registration.endpoint}?state=${encodeURIComponent(flow.state)}`,
      manifest: JSON.stringify(registration.manifest),
    } };
  } catch {
    return { error: "Could not prepare registration. Check credential storage readiness and try again; no App was created by Kars.", registration: null };
  }
}

export async function disconnectGithubAppAction(_prev: GithubAppState, _form: FormData): Promise<GithubAppState> {
  try {
    await deleteGithubApp();
    revalidatePath("/console/configuration");
    return { error: null, ok: "Disconnected — the shared App credential was removed." };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "Disconnect failed.", ok: null };
  }
}
