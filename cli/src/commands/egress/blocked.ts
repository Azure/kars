// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `azureclaw egress blocked <sandbox> [--since 10m] [--top] [--watch]`
//! — surface the router's `/internal/egress/blocked` view of every
//! egress attempt that was denied by the enforcement layer.
//!
//! Slice 5a in
//! `docs/internal/crd-well-oiled-machine/slice-5-egress-polish-and-observability.md`.
//! Mirrors the in-pod curl approach used by `azureclaw inspect` so we
//! avoid `kubectl port-forward` port-collision footguns.

import { Command } from "commander";
import chalk from "chalk";
import { execa } from "execa";

// --- Wire DTOs (must mirror `inference-router/src/routes/internal.rs`) ----

export interface BlockedEntry {
  host: string;
  port: number;
  source_sandbox: string;
  count: number;
  first_seen_unix: number;
  last_seen_unix: number;
  first_seen: string;
  last_seen: string;
}

export interface BlockedResponse {
  schema_version: number;
  total: number;
  count: number;
  since_unix: number;
  entries: BlockedEntry[];
}

export interface TopHost {
  host: string;
  count: number;
}

export interface BlockedTopResponse {
  schema_version: number;
  since_unix: number;
  window: string;
  n: number;
  top: TopHost[];
}

interface BlockedOptions {
  namespace?: string;
  since?: string;
  top?: boolean;
  window?: string;
  n?: string;
  watch?: boolean;
  json?: boolean;
}

export function blockedCommand(): Command {
  return new Command("blocked")
    .description(
      "Show egress attempts the router's enforcement layer denied (Slice 5a)"
    )
    .argument("<sandbox>", "Sandbox name (the `metadata.name` of the ClawSandbox)")
    .option(
      "-n, --namespace <ns>",
      "Sandbox pod namespace (default: 'azureclaw-<sandbox>')"
    )
    .option(
      "--since <duration>",
      "Only show attempts newer than this. Accepts RFC 3339, Unix seconds, or '-Nm'/'-Nh'/'-Nd' (default: all-time)"
    )
    .option(
      "--top",
      "Show top-N most-attempted blocked hosts in a rolling window instead of the full list"
    )
    .option(
      "--window <duration>",
      "With --top: rolling window for the top-N aggregate. Examples: 5m, 1h, 24h (default: 5m)"
    )
    .option(
      "--n <count>",
      "With --top: how many hosts to surface (default: 10, max: 100)"
    )
    .option(
      "--watch",
      "Poll every 5s and re-render. Press Ctrl-C to exit."
    )
    .option("--json", "Emit raw JSON instead of the formatted table")
    .action(async (sandbox: string, opts: BlockedOptions) => {
      try {
        await runBlocked(sandbox, opts);
      } catch (e: unknown) {
        const msg = e instanceof Error ? e.message : String(e);
        console.error(chalk.red(`\n  egress blocked failed: ${msg}\n`));
        process.exitCode = 1;
      }
    });
}

async function runBlocked(sandbox: string, opts: BlockedOptions): Promise<void> {
  const ns = opts.namespace ?? `azureclaw-${sandbox}`;
  const token = await readAdminToken(sandbox, ns);
  if (!token) {
    throw new Error(
      `Could not read admin token from secret 'router-admin-token' in '${ns}'.\n` +
        `  The sandbox may not be fully provisioned yet. Try 'azureclaw status ${sandbox}'.`
    );
  }

  const renderOnce = async () => {
    const path = buildPath(opts);
    const raw = await fetchInternal(sandbox, ns, token, path);
    if (opts.json) {
      console.log(raw);
      return;
    }
    if (opts.top) {
      const resp = JSON.parse(raw) as BlockedTopResponse;
      renderTop(sandbox, ns, resp);
    } else {
      const resp = JSON.parse(raw) as BlockedResponse;
      renderList(sandbox, ns, resp);
    }
  };

  if (!opts.watch) {
    await renderOnce();
    return;
  }

  // Render-loop: clear screen + redraw every 5s until SIGINT.
  // We don't use `setInterval` so a slow render can't double-stack.
  let stop = false;
  process.on("SIGINT", () => {
    stop = true;
  });
  while (!stop) {
    process.stdout.write("\x1Bc"); // VT clear
    try {
      await renderOnce();
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      console.error(chalk.yellow(`  fetch failed: ${msg} (retrying)`));
    }
    await sleep(5_000);
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

/// Compose the `/internal/egress/blocked[/top]` URL with the right query
/// string for the current invocation.
export function buildPath(opts: BlockedOptions): string {
  if (opts.top) {
    const params = new URLSearchParams();
    if (opts.window) params.set("window", opts.window);
    if (opts.n) params.set("n", opts.n);
    const qs = params.toString();
    return qs
      ? `/internal/egress/blocked/top?${qs}`
      : "/internal/egress/blocked/top";
  }
  const params = new URLSearchParams();
  if (opts.since) params.set("since", opts.since);
  const qs = params.toString();
  return qs ? `/internal/egress/blocked?${qs}` : "/internal/egress/blocked";
}

async function readAdminToken(
  sandbox: string,
  ns: string
): Promise<string | undefined> {
  try {
    const { stdout } = await execa(
      "kubectl",
      [
        "get",
        "secret",
        "router-admin-token",
        "-n",
        ns,
        "-o",
        "jsonpath={.data.token}",
      ],
      { stdio: "pipe", reject: false }
    );
    if (stdout.trim()) {
      return Buffer.from(stdout.trim(), "base64").toString("utf8").trim();
    }
  } catch {
    /* fall through */
  }
  for (const container of ["inference-router", "openclaw"]) {
    try {
      const { stdout } = await execa(
        "kubectl",
        [
          "exec",
          "-n",
          ns,
          `deploy/${sandbox}`,
          "-c",
          container,
          "--",
          "cat",
          "/etc/azureclaw/secrets/admin-token",
        ],
        { stdio: "pipe", reject: false }
      );
      if (stdout.trim()) return stdout.trim();
    } catch {
      /* try next */
    }
  }
  return undefined;
}

/// In-pod curl, token via stdin (no argv leak).
async function fetchInternal(
  sandbox: string,
  ns: string,
  token: string,
  path: string
): Promise<string> {
  const script =
    "read -r AZURECLAW_ADMIN_TOKEN <&0 && " +
    "curl --silent --show-error --fail --max-time 10 " +
    '-H "Authorization: Bearer $AZURECLAW_ADMIN_TOKEN" ' +
    `http://127.0.0.1:8443${path}`;

  const result = await execa(
    "kubectl",
    [
      "exec",
      "-i",
      "-n",
      ns,
      `deploy/${sandbox}`,
      "-c",
      "openclaw",
      "--",
      "bash",
      "-c",
      script,
    ],
    {
      stdio: ["pipe", "pipe", "pipe"],
      input: `${token}\n`,
      reject: false,
    }
  );

  if (result.exitCode !== 0) {
    throw new Error(
      `Router ${path} fetch failed (exit ${result.exitCode}). ` +
        `stderr: ${result.stderr?.toString().trim() ?? "<empty>"}`
    );
  }
  return result.stdout;
}

// --- Renderers ------------------------------------------------------------

export function renderList(
  sandbox: string,
  ns: string,
  resp: BlockedResponse
): void {
  console.log("");
  console.log(
    chalk.bold(`  Blocked egress attempts — ${sandbox}`) +
      chalk.dim(` (namespace: ${ns})`)
  );
  if (resp.since_unix > 0) {
    console.log(chalk.dim(`  Filter: since ${unixToIso(resp.since_unix)}`));
  }
  console.log(
    chalk.dim(`  ${resp.count} of ${resp.total} buffered entries shown\n`)
  );
  if (resp.entries.length === 0) {
    console.log(chalk.green("  ✓ No blocked attempts in the window.\n"));
    return;
  }
  const rows = resp.entries.map((e) => [
    e.host,
    String(e.port),
    e.source_sandbox,
    e.last_seen,
    String(e.count),
  ]);
  printTable(["HOST", "PORT", "SANDBOX", "LAST_SEEN", "COUNT"], rows);
  console.log("");
}

export function renderTop(
  sandbox: string,
  ns: string,
  resp: BlockedTopResponse
): void {
  console.log("");
  console.log(
    chalk.bold(`  Top blocked hosts — ${sandbox}`) +
      chalk.dim(` (namespace: ${ns})`)
  );
  console.log(
    chalk.dim(
      `  Window: ${resp.window} (since ${unixToIso(resp.since_unix)}), top ${resp.n}\n`
    )
  );
  if (resp.top.length === 0) {
    console.log(chalk.green("  ✓ No blocked attempts in the window.\n"));
    return;
  }
  const rows = resp.top.map((t) => [t.host, String(t.count)]);
  printTable(["HOST", "COUNT"], rows);
  console.log("");
}

export function printTable(headers: string[], rows: string[][]): void {
  const widths = headers.map((h, i) =>
    Math.max(h.length, ...rows.map((r) => (r[i] ?? "").length))
  );
  const sep = "  ";
  const header = headers
    .map((h, i) => h.padEnd(widths[i]))
    .join(sep);
  console.log("  " + chalk.bold(header));
  for (const row of rows) {
    console.log("  " + row.map((c, i) => c.padEnd(widths[i])).join(sep));
  }
}

export function unixToIso(unix: number): string {
  if (unix === 0) return "epoch";
  // Slice 5a router emits `.000Z`; mirror that for consistency.
  return new Date(unix * 1000).toISOString().replace(/\.\d{3}Z$/, ".000Z");
}
