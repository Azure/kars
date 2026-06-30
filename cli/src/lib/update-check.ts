// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// lib/update-check.ts — best-effort "a newer CLI is published" notifier.
//
// Every interactive `kars` run quietly checks whether a newer
// `@kars-runtime/cli` has been published to npm and, if so, prints a short
// notice (current → latest + a one-line changelog) and offers to install it.
// Design constraints, in priority order:
//   1. NEVER break or slow a command. The check is wrapped in a hard timeout,
//      all errors are swallowed, and the result is cached so we hit the network
//      at most once per `CHECK_TTL_MS` window.
//   2. NEVER nag. We surface the notice at most once per `CHECK_TTL_MS`, only on
//      a TTY, never in CI, and respect an explicit opt-out env var.
//   3. NEVER act without consent. The install only runs if the user confirms the
//      interactive prompt; otherwise we just print the copy-paste command.
//
// The cache lives at `~/.kars/update-check.json`. It records the last fetched
// latest version, when we fetched it, and when we last showed the notice — so
// the notice is decoupled from the network check (classic update-notifier
// behaviour: we may show a previously-fetched result while refreshing for next
// time).

import { promises as fs } from "node:fs";
import os from "node:os";
import path from "node:path";
import { cliVersion } from "./version.js";
import { compareVersions } from "./release.js";

/** npm package this CLI is published as. */
export const CLI_PACKAGE = "@kars-runtime/cli";

/** How long a network result / shown-notice is considered fresh. */
export const CHECK_TTL_MS = 24 * 60 * 60 * 1000; // 24h

/** Hard cap on the registry request so a slow/unreachable network never delays
 *  the user's actual command by more than this. */
const FETCH_TIMEOUT_MS = 1500;

const CACHE_DIR = path.join(os.homedir(), ".kars");
const CACHE_FILE = path.join(CACHE_DIR, "update-check.json");

/** Resolve the cache file, honouring `KARS_STATE_DIR` so tests (and unusual
 *  environments) can redirect the on-disk cache away from `~/.kars`. */
function cacheFile(): string {
  const override = process.env.KARS_STATE_DIR;
  return override ? path.join(override, "update-check.json") : CACHE_FILE;
}

interface UpdateCache {
  /** Latest version string seen on the registry (no `v` prefix), or null. */
  latest: string | null;
  /** Epoch ms of the last successful (or attempted) registry fetch. */
  checkedAt: number;
  /** Epoch ms we last printed the notice to the user. */
  notifiedAt: number;
}

export interface UpdateInfo {
  current: string;
  latest: string;
  /** Short changelog blurb for `latest`, when one is cheaply available. */
  changelog?: string;
}

/** True when update checks are disabled for this process: explicit opt-out, CI,
 *  or a non-interactive stdout (piped / redirected). Exported for tests. */
export function updateCheckDisabled(env: NodeJS.ProcessEnv = process.env): boolean {
  if (env.KARS_NO_UPDATE_CHECK === "1" || env.KARS_NO_UPDATE_CHECK === "true") return true;
  if (env.KARS_UPDATE_CHECK === "0" || env.KARS_UPDATE_CHECK === "false") return true;
  // Common CI signal — never nag an automated pipeline.
  if (env.CI && env.CI !== "0" && env.CI !== "false") return true;
  return false;
}

async function readCache(): Promise<UpdateCache | null> {
  try {
    const raw = await fs.readFile(cacheFile(), "utf8");
    const parsed = JSON.parse(raw) as Partial<UpdateCache>;
    if (typeof parsed.checkedAt !== "number") return null;
    return {
      latest: typeof parsed.latest === "string" ? parsed.latest : null,
      checkedAt: parsed.checkedAt,
      notifiedAt: typeof parsed.notifiedAt === "number" ? parsed.notifiedAt : 0,
    };
  } catch {
    return null;
  }
}

async function writeCache(cache: UpdateCache): Promise<void> {
  try {
    const file = cacheFile();
    await fs.mkdir(path.dirname(file), { recursive: true });
    await fs.writeFile(file, JSON.stringify(cache), "utf8");
  } catch {
    // Cache is an optimisation; a write failure must never surface.
  }
}

/** Fetch the `latest` dist-tag version from the npm registry. Best-effort:
 *  returns null on any error or timeout. Exported for tests. */
export async function fetchLatestVersion(
  pkg: string = CLI_PACKAGE,
  timeoutMs: number = FETCH_TIMEOUT_MS,
): Promise<string | null> {
  // The `latest` endpoint returns just the dist-tag manifest — far smaller than
  // the full packument.
  const url = `https://registry.npmjs.org/${pkg.replace("/", "%2f")}/latest`;
  try {
    const resp = await fetch(url, {
      signal: AbortSignal.timeout(timeoutMs),
      headers: { accept: "application/json" },
    });
    if (!resp.ok) return null;
    const body = (await resp.json()) as { version?: string };
    return typeof body.version === "string" ? body.version : null;
  } catch {
    return null;
  }
}

/** A terse, best-effort one-line changelog for `version`, pulled from the
 *  GitHub release body. Returns undefined when unavailable. Exported for tests. */
export async function fetchChangelogSummary(
  version: string,
  timeoutMs: number = FETCH_TIMEOUT_MS,
): Promise<string | undefined> {
  const tag = version.startsWith("v") ? version : `v${version}`;
  const url = `https://api.github.com/repos/Azure/kars/releases/tags/${tag}`;
  try {
    const resp = await fetch(url, {
      signal: AbortSignal.timeout(timeoutMs),
      headers: { accept: "application/vnd.github+json" },
    });
    if (!resp.ok) return undefined;
    const body = (await resp.json()) as { name?: string; body?: string };
    return summarizeReleaseBody(body.name, body.body);
  } catch {
    return undefined;
  }
}

/** Reduce a GitHub release `name`/`body` to a single readable line. Prefers the
 *  release title; otherwise the first meaningful bullet/line of the body.
 *  Exported for tests. */
export function summarizeReleaseBody(name?: string, body?: string): string | undefined {
  const clean = (s: string): string =>
    s.replace(/^[#>*\-\s]+/, "").replace(/`/g, "").replace(/\s+/g, " ").trim();
  if (name && name.trim()) {
    // Release titles are often "vX.Y.Z — summary"; keep the summary half.
    const dash = name.split(/\s[—-]\s/);
    const tail = dash.length > 1 ? dash.slice(1).join(" - ") : name;
    const t = clean(tail);
    if (t) return truncate(t);
  }
  if (body) {
    for (const line of body.split(/\r?\n/)) {
      // Skip markdown headings ("## What's changed") — we want a content line.
      if (/^\s*#/.test(line)) continue;
      const c = clean(line);
      if (c.length >= 8) return truncate(c);
    }
  }
  return undefined;
}

function truncate(s: string, max = 100): string {
  return s.length > max ? `${s.slice(0, max - 1).trimEnd()}…` : s;
}

/**
 * Determine whether an update is available, using the cache to avoid hitting the
 * network more than once per TTL. Returns null when up-to-date, disabled, or the
 * lookup couldn't determine a newer version.
 *
 * `force` bypasses the TTL (used by the explicit `kars update` command).
 */
export async function checkForCliUpdate(opts: {
  force?: boolean;
  withChangelog?: boolean;
  env?: NodeJS.ProcessEnv;
} = {}): Promise<UpdateInfo | null> {
  const env = opts.env ?? process.env;
  if (!opts.force && updateCheckDisabled(env)) return null;

  const current = cliVersion();
  const now = Date.now();
  const cache = await readCache();

  let latest = cache?.latest ?? null;
  const fresh = cache && now - cache.checkedAt < CHECK_TTL_MS;

  if (opts.force || !fresh) {
    const fetched = await fetchLatestVersion();
    // Record the attempt regardless of outcome so a flaky network doesn't make
    // us re-check on every single invocation.
    latest = fetched ?? latest;
    await writeCache({
      latest,
      checkedAt: now,
      notifiedAt: cache?.notifiedAt ?? 0,
    });
  }

  if (!latest) return null;
  if (compareVersions(latest, current) <= 0) return null;

  const info: UpdateInfo = { current, latest };
  if (opts.withChangelog) {
    info.changelog = await fetchChangelogSummary(latest);
  }
  return info;
}

/** Record that we've shown the notice now, so we don't repeat it within the TTL. */
async function markNotified(): Promise<void> {
  const cache = (await readCache()) ?? { latest: null, checkedAt: Date.now(), notifiedAt: 0 };
  cache.notifiedAt = Date.now();
  await writeCache(cache);
}

/** True if we've already shown a notice within the TTL window. */
async function notifiedRecently(): Promise<boolean> {
  const cache = await readCache();
  return !!cache && Date.now() - cache.notifiedAt < CHECK_TTL_MS;
}

/**
 * The passive, end-of-run hook wired into every `kars` invocation. Best-effort:
 * checks for an update (cached) and, at most once per TTL on an interactive
 * terminal, offers to install it. Silent and side-effect-free when up-to-date,
 * disabled, non-interactive, or already shown recently.
 *
 * Returns true if a notice was shown (used by tests).
 */
export async function maybeNotifyCliUpdate(): Promise<boolean> {
  try {
    if (updateCheckDisabled()) return false;
    if (await notifiedRecently()) return false;

    const info = await checkForCliUpdate({ withChangelog: true });
    if (!info) return false;

    await markNotified();
    // The passive notice never hijacks stdin after an arbitrary command — it
    // prints the changelog + how to upgrade. The interactive install lives in
    // `kars update`, which the notice points to.
    await renderUpdateNotice(info, { offerInstall: false });
    return true;
  } catch {
    return false;
  }
}

/**
 * Render the update notice. On an interactive TTY (`offerInstall`) it prompts to
 * run the global install; otherwise it just prints the copy-paste command.
 * Exported so the `kars update` command can reuse the exact same presentation.
 */
export async function renderUpdateNotice(
  info: UpdateInfo,
  opts: { offerInstall?: boolean } = {},
): Promise<void> {
  const { default: chalk } = await import("chalk");
  const installCmd = `npm install -g ${CLI_PACKAGE}@latest`;

  const lines = [
    "",
    chalk.cyan(`  ✨ A new kars CLI is available: `) +
      chalk.dim(info.current) + chalk.cyan("  →  ") + chalk.green.bold(info.latest),
  ];
  if (info.changelog) {
    lines.push(chalk.dim(`     ${info.changelog}`));
  }
  lines.push(
    chalk.dim(`     Release notes: https://github.com/Azure/kars/releases/tag/v${info.latest}`),
    "",
  );
  console.error(lines.join("\n"));

  if (opts.offerInstall) {
    const { default: inquirer } = await import("inquirer");
    const { doInstall } = await inquirer.prompt<{ doInstall: boolean }>([
      {
        type: "confirm",
        name: "doInstall",
        message: `Update to ${info.latest} now?`,
        default: true,
      },
    ]);
    if (doInstall) {
      await runGlobalInstall();
      return;
    }
  }
  console.error(
    chalk.dim(`  Run `) + chalk.bold(`kars update`) +
      chalk.dim(` to install it, or: `) + chalk.bold(installCmd) + "\n",
  );
}

/** Run the global npm install of the latest CLI, streaming output. Best-effort:
 *  reports failure with the manual command rather than throwing. */
export async function runGlobalInstall(): Promise<void> {
  const { default: chalk } = await import("chalk");
  const { execa } = await import("execa");
  const installCmd = `npm install -g ${CLI_PACKAGE}@latest`;
  try {
    console.error(chalk.dim(`\n  $ ${installCmd}\n`));
    await execa("npm", ["install", "-g", `${CLI_PACKAGE}@latest`], { stdio: "inherit" });
    console.error(chalk.green(`\n  ✓ Updated. Re-run your command to use ${CLI_PACKAGE}@latest.\n`));
  } catch {
    console.error(
      chalk.yellow(`\n  ⚠ Automatic update failed. Update manually with:\n  `) +
        chalk.bold(installCmd) + "\n",
    );
  }
}
