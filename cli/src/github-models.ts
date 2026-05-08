// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * GitHub Models integration helpers.
 *
 * Used by the first-run onboarding flow and the `azureclaw config` command
 * tree to:
 *   - detect logged-in `gh` accounts (multi-account aware)
 *   - fetch the live catalog and validate a PAT scope
 *   - present a curated model picker with escape hatches
 *   - normalize stored secret values so all write paths behave identically
 *
 * Stays self-contained so it's easy to mock in tests and swap out per-tier
 * recommendations without touching `config.ts`.
 */

import chalk from "chalk";

export const GITHUB_MODELS_ENDPOINT = "https://models.github.ai/inference";
const CATALOG_URL = "https://models.github.ai/catalog/models";

export interface CatalogModel {
  id: string;
  name?: string;
  publisher?: string;
  summary?: string;
  rate_limit_tier?: "low" | "high" | "custom" | "embeddings" | string;
  capabilities?: string[];
  limits?: { max_input_tokens?: number; max_output_tokens?: number };
  supported_input_modalities?: string[];
  supported_output_modalities?: string[];
  tags?: string[];
}

/**
 * Curated short-list shown on the picker. Hand-picked for agentic / tool-use
 * fitness — not algorithmically ranked. Order = display order. Entries are
 * filtered against the live catalog at runtime so a model dropping
 * `tool-calling` (or vanishing entirely) auto-disappears.
 */
export interface RecommendedEntry {
  id: string;
  /** "free" = free tier on GitHub Models, "paid" = requires paid plan */
  tier: "free" | "paid";
  /** Short note shown in the picker */
  note: string;
  /** True for the highlighted default */
  recommended?: boolean;
}

export const RECOMMENDED_MODELS: RecommendedEntry[] = [
  { id: "openai/gpt-4.1",                              tier: "free", note: "recommended for agents", recommended: true },
  { id: "openai/gpt-4.1-mini",                         tier: "free", note: "faster, cheaper quota" },
  { id: "meta/llama-4-maverick-17b-128e-instruct-fp8", tier: "free", note: "open weights" },
  { id: "deepseek/deepseek-v3-0324",                   tier: "free", note: "strong reasoning" },
  { id: "openai/gpt-5",                                tier: "paid", note: "flagship · 200k ctx" },
  { id: "openai/gpt-5-mini",                           tier: "paid", note: "premium price/perf" },
];

/**
 * Format the catalog tier for display. Tolerant to schema drift.
 */
export function tierLabel(tier?: string): "free" | "paid" | "embed" | "unknown" {
  if (tier === "low" || tier === "high") return "free";
  if (tier === "custom") return "paid";
  if (tier === "embeddings") return "embed";
  return "unknown";
}

/**
 * Returns true when the catalog model can be used by an AzureClaw agent
 * (must support tool/function calling).
 */
export function isToolCapable(m: CatalogModel): boolean {
  const caps = m.capabilities ?? [];
  // Tolerate schema drift — match either the documented "tool-calling" or
  // the upstream legacy "tools" capability if it ever returns.
  return caps.includes("tool-calling") || caps.includes("tools");
}

export interface FetchCatalogResult {
  ok: true;
  models: CatalogModel[];
}
export interface FetchCatalogError {
  ok: false;
  status: number;
  message: string;
}

/**
 * Fetch the GitHub Models catalog. The endpoint also serves as the
 * authoritative scope check for a PAT — a 200 here proves `models:read`.
 * `/user` does NOT prove Models access, so we never use it for scope.
 */
export async function fetchCatalog(pat: string): Promise<FetchCatalogResult | FetchCatalogError> {
  try {
    const resp = await fetch(CATALOG_URL, {
      method: "GET",
      headers: {
        "Authorization": `Bearer ${pat}`,
        "Accept": "application/vnd.github+json",
        "User-Agent": "azureclaw-cli",
      },
    });
    if (!resp.ok) {
      const body = await resp.text();
      return { ok: false, status: resp.status, message: body.slice(0, 500) };
    }
    const data = (await resp.json()) as CatalogModel[];
    if (!Array.isArray(data)) {
      return { ok: false, status: 0, message: "Catalog response was not an array" };
    }
    return { ok: true, models: data };
  } catch (e) {
    return { ok: false, status: 0, message: e instanceof Error ? e.message : String(e) };
  }
}

/**
 * Build the picker choice list from the live catalog. Always:
 *   - filters to tool-capable models
 *   - drops curated entries that aren't in the catalog any more
 *   - groups paid behind a divider
 *   - appends "show all" + "enter custom id" escape hatches
 */
export interface PickerChoice {
  label: string;
  value: string;
  isDivider?: boolean;
}

export function buildCuratedChoices(
  catalog: CatalogModel[],
  current?: string,
): PickerChoice[] {
  const byId = new Map(catalog.map(m => [m.id, m]));
  const out: PickerChoice[] = [];
  const free = RECOMMENDED_MODELS.filter(r => r.tier === "free" && byId.has(r.id) && isToolCapable(byId.get(r.id)!));
  const paid = RECOMMENDED_MODELS.filter(r => r.tier === "paid" && byId.has(r.id) && isToolCapable(byId.get(r.id)!));

  for (const r of free) {
    out.push({ label: formatLine(byId.get(r.id)!, r, current), value: r.id });
  }
  if (paid.length > 0) {
    out.push({ label: "─── premium (paid plan; smaller context — may 413 with full tool catalogs) ───", value: "__divider_paid__", isDivider: true });
    for (const r of paid) {
      out.push({ label: formatLine(byId.get(r.id)!, r, current), value: r.id });
    }
  }
  out.push({ label: "─── more options ───", value: "__divider_more__", isDivider: true });
  out.push({ label: "Show all tool-capable models",        value: "__show_all__" });
  out.push({ label: "Enter custom model id",                value: "__custom__" });
  return out;
}

function formatLine(m: CatalogModel, r: RecommendedEntry, current?: string): string {
  const tag = r.tier === "free" ? chalk.green("free") : chalk.yellow("paid");
  const star = current === r.id ? chalk.cyan(" ★") : (r.recommended ? chalk.dim(" (recommended)") : "");
  const ctx = m.limits?.max_input_tokens
    ? chalk.dim(` · ${formatCtx(m.limits.max_input_tokens)} ctx`)
    : "";
  return `${m.id.padEnd(56)} ${tag}${ctx} · ${chalk.dim(r.note)}${star}`;
}

function formatCtx(tokens: number): string {
  if (tokens >= 1_000_000) return `${Math.round(tokens / 100_000) / 10}M`;
  if (tokens >= 1_000) return `${Math.round(tokens / 1000)}k`;
  return String(tokens);
}

/**
 * Build the "show all" picker list — every tool-capable model, grouped by
 * publisher and annotated with tier.
 */
export function buildAllToolCapableChoices(catalog: CatalogModel[], current?: string): PickerChoice[] {
  const tool = catalog.filter(isToolCapable);
  // Group by publisher
  const byPub = new Map<string, CatalogModel[]>();
  for (const m of tool) {
    const p = m.publisher ?? "Unknown";
    if (!byPub.has(p)) byPub.set(p, []);
    byPub.get(p)!.push(m);
  }
  const out: PickerChoice[] = [];
  for (const pub of [...byPub.keys()].sort()) {
    out.push({ label: `─── ${pub} ───`, value: `__divider_${pub}__`, isDivider: true });
    for (const m of byPub.get(pub)!.sort((a, b) => a.id.localeCompare(b.id))) {
      const tag = tierLabel(m.rate_limit_tier);
      const tagColored = tag === "free" ? chalk.green("free") : tag === "paid" ? chalk.yellow("paid") : chalk.dim(tag);
      const ctx = m.limits?.max_input_tokens ? chalk.dim(` · ${formatCtx(m.limits.max_input_tokens)} ctx`) : "";
      const star = current === m.id ? chalk.cyan(" ★") : "";
      out.push({ label: `${m.id.padEnd(56)} ${tagColored}${ctx}${star}`, value: m.id });
    }
  }
  return out;
}

/**
 * Validate a model id against the catalog. Returns `ok` if the model is
 * present AND tool-capable. Otherwise returns a suggestion when one of the
 * curated models has a similar prefix (e.g. user typed `gpt-5.4` — suggest
 * `openai/gpt-5-mini`).
 */
export interface ValidationOk { ok: true; model: CatalogModel; }
export interface ValidationFail { ok: false; reason: "not-found" | "no-tools"; suggestion?: string; }
export type ModelValidation = ValidationOk | ValidationFail;

export function validateModelAgainstCatalog(modelId: string, catalog: CatalogModel[]): ModelValidation {
  const m = catalog.find(c => c.id === modelId);
  if (!m) {
    const suggestion = closestSuggestion(modelId, catalog);
    return { ok: false, reason: "not-found", suggestion };
  }
  if (!isToolCapable(m)) {
    return { ok: false, reason: "no-tools", suggestion: undefined };
  }
  return { ok: true, model: m };
}

/**
 * Cheap-and-cheerful suggestion finder. Picks the curated entry whose id
 * shares the longest common prefix (case-insensitive) with the user's
 * input; falls back to any catalog model. Skips entries that aren't tool-
 * capable.
 */
function closestSuggestion(input: string, catalog: CatalogModel[]): string | undefined {
  const needle = input.toLowerCase();
  // Canonicalize: drop everything before "/" (so "gpt-5.4" matches "openai/gpt-…")
  const tail = needle.includes("/") ? needle.split("/").pop()! : needle;
  // Normalize separators that users typo (`.` vs `-`)
  const norm = tail.replace(/[._]/g, "-");
  let bestId: string | undefined;
  let bestScore = 0;
  const pool = catalog.filter(isToolCapable);
  for (const m of pool) {
    const idTail = (m.id.split("/").pop() ?? m.id).toLowerCase().replace(/[._]/g, "-");
    let score = 0;
    while (score < norm.length && score < idTail.length && norm[score] === idTail[score]) score++;
    if (score > bestScore) {
      bestScore = score;
      bestId = m.id;
    }
  }
  // Require at least 3 matching chars to avoid nonsense suggestions
  return bestScore >= 3 ? bestId : undefined;
}

// ─── `gh` CLI integration ───────────────────────────────────────────────────

export interface GhAccount {
  login: string;
  active: boolean;
  scopes: string[];
  tokenSource: string;
  host: string;
}

/**
 * Detect logged-in `gh` accounts on github.com. Returns [] when `gh` isn't
 * installed, isn't logged in, or returns malformed JSON.
 */
export async function detectGhAccounts(): Promise<GhAccount[]> {
  const { execa } = await import("execa");
  try {
    // `gh auth status --json hosts` is reliable since gh 2.40+ and supersedes
    // the (brittle) text-format parser. Empty hosts array means no logins.
    const result = await execa("gh", ["auth", "status", "--hostname", "github.com", "--json", "hosts"], {
      stdio: "pipe",
      reject: false,
    });
    if (result.exitCode !== 0 || !result.stdout) return [];
    const parsed = JSON.parse(result.stdout) as { hosts?: { [k: string]: Array<{ login?: string; active?: boolean; scopes?: string; tokenSource?: string; host?: string; state?: string }> } };
    const entries = parsed.hosts?.["github.com"] ?? [];
    return entries
      .filter(e => e.state === "success" && typeof e.login === "string")
      .map(e => ({
        login: e.login!,
        active: !!e.active,
        scopes: typeof e.scopes === "string" ? e.scopes.split(",").map(s => s.trim()).filter(Boolean) : [],
        tokenSource: e.tokenSource ?? "unknown",
        host: e.host ?? "github.com",
      }));
  } catch {
    return [];
  }
}

/**
 * Get the OAuth token for a specific gh account. Returns `null` if `gh`
 * fails or returns whitespace.
 */
export async function getGhToken(login?: string): Promise<string | null> {
  const { execa } = await import("execa");
  try {
    const args = ["auth", "token", "--hostname", "github.com"];
    if (login) args.push("--user", login);
    const result = await execa("gh", args, { stdio: "pipe", reject: false });
    if (result.exitCode !== 0) return null;
    const tok = (result.stdout ?? "").trim();
    if (!tok || /\s/.test(tok)) return null;
    return tok;
  } catch {
    return null;
  }
}

// ─── Secret normalization ──────────────────────────────────────────────────

/**
 * Apply key-specific normalization to a secret value before it lands in
 * secrets.json. Centralized here so every write path (set subcommand,
 * interactive credentials menu, dev gap-fill) gets the same behavior.
 *
 * Currently:
 *   - `telegram-token` (and dot-suffix variants): strip leading `bot` so
 *     we don't end up with `botbot…` after grammY prefixes its own `bot`.
 */
export function normalizeSecretValue(key: string, value: string): string {
  const baseKey = key.includes(".") ? key.slice(0, key.indexOf(".")) : key;
  let v = value.trim();
  if (baseKey === "telegram-token" && v.startsWith("bot")) {
    v = v.slice(3);
  }
  return v;
}

// ─── PAT creation deep-link ─────────────────────────────────────────────────

/**
 * Deep-link to GitHub's PAT creation page with description/scope pre-filled.
 * Note: classic PATs use `models:read`. Fine-grained PATs configure Models
 * access through the "Models" permission UI on the fine-grained page; we
 * keep the classic link as the simpler default.
 */
export const PAT_CREATE_URL =
  "https://github.com/settings/tokens/new?description=AzureClaw%20CLI&scopes=read:user";

/**
 * Format a "rate-limit hit" hint for the user when chat fails on a tier
 * boundary (paid `custom`-tier model called by a free-tier PAT).
 */
export function quotaHelpText(modelId: string): string {
  return `${modelId} requires a paid GitHub Models plan. Run ${chalk.cyan("azureclaw config model")} to switch to a free-tier model.`;
}
