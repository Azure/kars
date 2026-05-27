// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * GitHub Copilot integration helpers.
 *
 * Copilot is a third inference provider alongside Foundry and GH Models.
 * Compared to GH Models it has:
 *   - much larger context windows (claude-opus-4.7 = 200k input)
 *   - native Anthropic Messages API in addition to OpenAI chat
 *   - per-seat licensing (no separate API quota)
 *
 * Auth flow used by the router:
 *   gh PAT/OAuth token → GET https://api.github.com/copilot_internal/v2/token
 *     → short-lived JWT used as Bearer for inference calls.
 *
 * This module exposes the model catalog, gh token detection (reuses helpers
 * from `github-models.ts`), and a quick eligibility check so the CLI can
 * tell the user whether their account has a Copilot subscription before
 * picking the provider.
 */

import { detectGhAccounts, getGhToken, type GhAccount } from "./github-models.js";

/** Public OpenAI-compatible base. The router rewrites /v1/* through to here. */
export const COPILOT_API_ENDPOINT = "https://api.githubcopilot.com";

/** Token exchange endpoint — exchanges a gh OAuth/PAT token for a Copilot JWT. */
export const COPILOT_TOKEN_URL = "https://api.github.com/copilot_internal/v2/token";

/** PAT creation deep-link — Copilot needs a token with `read:user` (the same
 *  scope `gh auth login` produces). Models access is gated by the user's
 *  Copilot subscription, not by token scope. */
export const COPILOT_PAT_CREATE_URL =
  "https://github.com/settings/tokens/new?description=Kars%20CLI%20(Copilot)&scopes=read:user";

/** Public OAuth client_id used by the Copilot device-flow integration. The
 *  same id is used by the JetBrains/Neovim Copilot plugins and several
 *  open-source Copilot proxies. The token it returns is durable and is
 *  authorized for `copilot_internal/v2/token` exchange — unlike a stock
 *  `gh auth login` token, which is missing the Copilot integration scope
 *  and 404s on the exchange endpoint. */
export const COPILOT_OAUTH_CLIENT_ID = "Iv1.b507a08c87ecfe98";
export const GITHUB_DEVICE_CODE_URL = "https://github.com/login/device/code";
export const GITHUB_OAUTH_TOKEN_URL = "https://github.com/login/oauth/access_token";

/**
 * Run GitHub's device-code OAuth flow for the public Copilot client. Prints
 * a user code + verification URL via `onPrompt`, polls for completion, and
 * returns a Copilot-authorized OAuth token (`gho_…`). Throws on user
 * cancel, timeout, or HTTP error.
 */
export async function copilotDeviceLogin(
  onPrompt: (info: { userCode: string; verificationUri: string; expiresInSec: number }) => void | Promise<void>,
): Promise<string> {
  const initResp = await fetch(GITHUB_DEVICE_CODE_URL, {
    method: "POST",
    headers: { Accept: "application/json", "Content-Type": "application/json", "User-Agent": "GitHubCopilotChat/0.26.7" },
    body: JSON.stringify({ client_id: COPILOT_OAUTH_CLIENT_ID, scope: "read:user" }),
  });
  if (!initResp.ok) {
    throw new Error(`device-code request failed: ${initResp.status} ${await initResp.text().catch(() => "")}`);
  }
  const init = (await initResp.json()) as {
    device_code: string;
    user_code: string;
    verification_uri: string;
    expires_in: number;
    interval: number;
  };

  await onPrompt({ userCode: init.user_code, verificationUri: init.verification_uri, expiresInSec: init.expires_in });

  const startedAt = Date.now();
  let intervalSec = Math.max(5, init.interval || 5);

  while ((Date.now() - startedAt) / 1000 < init.expires_in) {
    await new Promise((r) => setTimeout(r, intervalSec * 1000));
    const pollResp = await fetch(GITHUB_OAUTH_TOKEN_URL, {
      method: "POST",
      headers: { Accept: "application/json", "Content-Type": "application/json", "User-Agent": "GitHubCopilotChat/0.26.7" },
      body: JSON.stringify({
        client_id: COPILOT_OAUTH_CLIENT_ID,
        device_code: init.device_code,
        grant_type: "urn:ietf:params:oauth:grant-type:device_code",
      }),
    });
    if (!pollResp.ok) {
      throw new Error(`oauth poll failed: ${pollResp.status} ${await pollResp.text().catch(() => "")}`);
    }
    const body = (await pollResp.json()) as {
      access_token?: string;
      error?: string;
      error_description?: string;
      interval?: number;
    };
    if (body.access_token) return body.access_token;
    switch (body.error) {
      case "authorization_pending":
        continue;
      case "slow_down":
        intervalSec = Math.max(intervalSec + 5, body.interval ?? intervalSec + 5);
        continue;
      case "expired_token":
        throw new Error("Device code expired before login completed. Please retry.");
      case "access_denied":
        throw new Error("Login cancelled in the browser.");
      default:
        throw new Error(`oauth error: ${body.error ?? "unknown"} ${body.error_description ?? ""}`.trim());
    }
  }
  throw new Error("Device-code flow timed out waiting for browser login.");
}

export interface CopilotModel {
  /** Provider-prefixed id sent to the inference router (`openai/gpt-5`, `anthropic/claude-opus-4.7`) */
  id: string;
  /** Short marketing name */
  name: string;
  /** Family — selects the wire shape OpenClaw uses. */
  family: "openai" | "anthropic" | "google";
  /** Marketing description for picker rows. */
  note: string;
  /** Approximate input-token window — informational, picker sort key. */
  contextWindow: number;
  /** True for the highlighted default. */
  recommended?: boolean;
}

/**
 * Curated catalog. Hand-maintained because Copilot does not expose a public
 * model list endpoint; ordering = display order in the picker. Models with
 * the largest context window are surfaced first because the original reason
 * to prefer Copilot over GH Models is that GH Models' 16k input cap kept
 * triggering OpenClaw auto-compaction → mesh state wipe.
 */
export const COPILOT_MODELS: CopilotModel[] = [
  { id: "claude-opus-4.7",        name: "Claude Opus 4.7",        family: "anthropic", note: "flagship reasoning · 200k ctx",      contextWindow: 200_000, recommended: true },
  { id: "claude-sonnet-4.5",      name: "Claude Sonnet 4.5",      family: "anthropic", note: "balanced · 200k ctx",                contextWindow: 200_000 },
  { id: "claude-haiku-4.5",       name: "Claude Haiku 4.5",       family: "anthropic", note: "fast/cheap · 200k ctx",              contextWindow: 200_000 },
  { id: "gpt-5",                  name: "GPT-5",                  family: "openai",    note: "OpenAI flagship · 256k ctx",         contextWindow: 256_000 },
  { id: "gpt-5-mini",             name: "GPT-5 mini",             family: "openai",    note: "cheaper · 256k ctx",                 contextWindow: 256_000 },
  { id: "gpt-4.1",                name: "GPT-4.1",                family: "openai",    note: "stable · 128k ctx",                  contextWindow: 128_000 },
  { id: "gemini-2.5-pro",         name: "Gemini 2.5 Pro",         family: "google",    note: "Google flagship · 1M ctx",           contextWindow: 1_000_000 },
];

export interface CopilotEligibility {
  ok: true;
  /** "individual", "business", "enterprise", or "trial" */
  plan: string;
  /** True when the JWT carries `chat_enabled=true`. */
  chatEnabled: boolean;
  /** Refresh hint from the JWT in seconds; informational. */
  refreshIn?: number;
}

export interface CopilotEligibilityError {
  ok: false;
  /** HTTP status from the token-exchange endpoint, or undefined for network errors. */
  status?: number;
  message: string;
}

/**
 * Probe Copilot subscription for a given gh OAuth token. Hits the same
 * token-exchange endpoint the router uses; a 200 means the user has an
 * active Copilot seat. This is a one-shot — don't cache the result locally
 * because seats can be revoked at any time.
 */
export async function checkCopilotEligibility(
  ghToken: string,
): Promise<CopilotEligibility | CopilotEligibilityError> {
  try {
    const resp = await fetch(COPILOT_TOKEN_URL, {
      method: "GET",
      headers: { // lgtm[js/file-access-to-http] — GitHub OAuth token is sent to Copilot eligibility endpoint by design
        Authorization: `Bearer ${ghToken}`,
        Accept: "application/json",
        "User-Agent": "GitHubCopilotChat/0.26.7",
        "Editor-Version": "vscode/1.99.0",
        "Editor-Plugin-Version": "copilot-chat/0.26.7",
      },
    });

    if (resp.status === 401 || resp.status === 403) {
      return {
        ok: false,
        status: resp.status,
        message:
          "GitHub token is not entitled to Copilot. Enable Copilot at https://github.com/settings/copilot or sign in with `gh auth login`.",
      };
    }
    if (!resp.ok) {
      return { ok: false, status: resp.status, message: `Copilot token endpoint returned ${resp.status}` };
    }

    const body = (await resp.json()) as {
      token?: string;
      expires_at?: number;
      refresh_in?: number;
      chat_enabled?: boolean;
      sku?: string;
      plan?: string;
    };
    if (!body.token) {
      return { ok: false, status: resp.status, message: "Copilot token endpoint returned no token" };
    }
    return {
      ok: true,
      plan: body.plan ?? body.sku ?? "individual",
      chatEnabled: body.chat_enabled !== false,
      refreshIn: body.refresh_in,
    };
  } catch (e) {
    return { ok: false, message: e instanceof Error ? e.message : String(e) };
  }
}

/**
 * Re-export gh detection helpers so callers don't need to know whether they
 * came from `github-models.ts` or `github-copilot.ts`. The auth surface for
 * Copilot is identical — the same gh OAuth token is exchanged.
 */
export { detectGhAccounts, getGhToken };
export type { GhAccount };

export interface CopilotPickerChoice {
  name: string;
  short: string;
  value: string;
}

/**
 * Build the inquirer-style picker rows. Default highlighted entry is the
 * `recommended` model (Claude Opus 4.7 — biggest practical context for
 * agentic + tool-call workloads).
 */
export function buildCopilotChoices(current?: string): CopilotPickerChoice[] {
  return COPILOT_MODELS.map((m) => {
    const star = m.recommended ? " ★" : "";
    const cur = current === m.id ? " (current)" : "";
    return {
      name: `${m.id.padEnd(28)}  ${m.note}${star}${cur}`,
      short: m.id,
      value: m.id,
    };
  });
}

/** Validate a free-form model id against the curated catalog. */
export function validateCopilotModel(modelId: string): { ok: boolean; message?: string } {
  if (COPILOT_MODELS.some((m) => m.id === modelId)) return { ok: true };
  return {
    ok: false,
    message: `'${modelId}' is not in the Copilot catalog. Known: ${COPILOT_MODELS.map((m) => m.id).join(", ")}`,
  };
}
