// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `kars inspect <sandbox>` — pretty-print the data-plane view of
//! every policy CRD bound to a sandbox.
//!
//! Hits the router's `GET /internal/policy-status` endpoint and prints
//! a per-`PolicyKind` summary: the digest the router has actually
//! loaded, when it was loaded, and the source path. This is the
//! operator-facing complement to `kubectl get karssandbox`: that
//! command shows the controller's view of the world (compiled digests,
//! phases). `inspect` shows the data plane's view — which is what
//! actually matters for "is my policy enforcing right now?".
//!
//! Slice 1d in `docs/internal/crd-well-oiled-machine/slice-1-toolpolicy-agt-and-shared-infra.md`.
//! The §7 mock-up in that doc shows the visual tree this command
//! produces.

import { Command } from "commander";
import chalk from "chalk";
import { execa } from "execa";

const POLICY_KIND_LABEL: Record<string, string> = {
  // Wire kinds emitted by `crate::policy_status::PolicyKind::as_str`.
  // Kept in sync with `inference-router/src/policy_status.rs`.
  ToolPolicy: "ToolPolicy",
  AgtProfile: "AGT profile",
  InferencePolicy: "InferencePolicy",
  Egress: "Egress",
  // Slice 3a: router emits `PolicyKind::Memory` (as_str="Memory") for
  // the compiled KarsMemory binding at /etc/kars/memory/binding.json.
  // Display as the user-facing CRD name.
  Memory: "KarsMemory",
};

interface PolicyStatusEntry {
  kind: string;
  digest: string | null;
  source_path: string;
  loaded_at: string;
  last_error: string | null;
}

interface PolicyStatusResponse {
  schema_version: number;
  count: number;
  entries: PolicyStatusEntry[];
}

interface InspectOptions {
  namespace?: string;
  json?: boolean;
}

export function inspectCommand(): Command {
  return new Command("inspect")
    .description(
      "Show the data-plane view of every policy CRD loaded by a sandbox's router"
    )
    .argument("<sandbox>", "Sandbox name (the `metadata.name` of the KarsSandbox)")
    .option(
      "-n, --namespace <ns>",
      "Sandbox pod namespace (default: 'kars-<sandbox>')"
    )
    .option("--json", "Emit raw JSON instead of the formatted tree")
    .action(async (sandbox: string, opts: InspectOptions) => {
      try {
        await runInspect(sandbox, opts);
      } catch (e: unknown) {
        const msg = e instanceof Error ? e.message : String(e);
        console.error(chalk.red(`\n  inspect failed: ${msg}\n`));
        process.exitCode = 1;
      }
    });
}

async function runInspect(
  sandbox: string,
  opts: InspectOptions
): Promise<void> {
  const ns = opts.namespace ?? `kars-${sandbox}`;

  const token = await readAdminToken(sandbox, ns);
  if (!token) {
    throw new Error(
      `Could not read admin token from secret 'router-admin-token' in '${ns}'.\n` +
        `  The sandbox may not be fully provisioned yet. Try 'kars status ${sandbox}'.`
    );
  }

  const response = await fetchPolicyStatus(sandbox, ns, token);

  if (opts.json) {
    console.log(JSON.stringify(response, null, 2));
    return;
  }

  renderTree(sandbox, ns, response);
}

/// Try every documented location for the admin token in priority order
/// (Slice 1's `router-admin-token` secret first, then the in-pod file
/// fallback). Returns `undefined` rather than throwing on a missing
/// secret so the caller can surface a more actionable error.
async function readAdminToken(
  sandbox: string,
  ns: string
): Promise<string | undefined> {
  // Preferred path — controller-managed Secret.
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
  // Fallback — read from inside the openclaw container (mount path
  // matches `handoff/helpers.ts:getAksAdminToken`).
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
          "/etc/kars/secrets/admin-token",
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

/// Execute the policy-status fetch inside the openclaw container via
/// `curl` rather than via `kubectl port-forward`. Port-forward suffers
/// from local-port collisions (see the conversation history in this
/// repo) and is harder to clean up on Ctrl-C; in-pod curl is a single
/// shot and inherits the pod's localhost loopback to the router on
/// :8443.
async function fetchPolicyStatus(
  sandbox: string,
  ns: string,
  token: string
): Promise<PolicyStatusResponse> {
  // Wrap the token in a shell variable instead of inlining it on the
  // command line — `ps`-visible argv would leak the secret. We pass
  // the token through stdin and let bash read it; --quiet keeps curl
  // off stderr so parsing stdout stays trivial.
  const script =
    'read -r KARS_ADMIN_TOKEN <&0 && ' +
    'curl --silent --show-error --fail --max-time 10 ' +
    '-H "Authorization: Bearer $KARS_ADMIN_TOKEN" ' +
    "http://127.0.0.1:8443/internal/policy-status";

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
      `Router /internal/policy-status fetch failed (exit ${result.exitCode}). ` +
        `stderr: ${result.stderr?.toString().trim() ?? "<empty>"}`
    );
  }

  try {
    return JSON.parse(result.stdout) as PolicyStatusResponse;
  } catch {
    throw new Error(
      `Router returned non-JSON response: ${result.stdout.slice(0, 200)}`
    );
  }
}

function renderTree(
  sandbox: string,
  ns: string,
  response: PolicyStatusResponse
): void {
  console.log("");
  console.log(
    chalk.bold(`  KarsSandbox: ${sandbox}`) + chalk.dim(` (${ns})`)
  );

  if (response.entries.length === 0) {
    console.log(
      chalk.dim(
        "  └── (no policies loaded — router has not registered any consumer yet)"
      )
    );
    console.log("");
    return;
  }

  // Group entries by kind so a single CRD with multiple artifacts
  // (e.g., ToolPolicy → tools.json + agt-profile.yaml after Slice 1b)
  // collapses under one header.
  const grouped = new Map<string, PolicyStatusEntry[]>();
  for (const entry of response.entries) {
    const key = POLICY_KIND_LABEL[entry.kind] ?? entry.kind;
    const bucket = grouped.get(key) ?? [];
    bucket.push(entry);
    grouped.set(key, bucket);
  }

  const kinds = [...grouped.keys()];
  kinds.forEach((kind, kindIdx) => {
    const isLastKind = kindIdx === kinds.length - 1;
    const prefix = isLastKind ? "  └──" : "  ├──";
    console.log(`${chalk.dim(prefix)} ${chalk.cyan(kind)}`);

    const entries = grouped.get(kind)!;
    entries.forEach((entry, entryIdx) => {
      const isLastEntry = entryIdx === entries.length - 1;
      const branch = isLastKind ? "    " : "  │ ";
      const tee = isLastEntry ? "└──" : "├──";
      console.log(
        `${chalk.dim(branch)}${chalk.dim(tee)} ${renderEntry(entry)}`
      );
    });
  });
  console.log("");
}

function renderEntry(entry: PolicyStatusEntry): string {
  const source = sourceLabel(entry.source_path);
  const digest = entry.digest
    ? chalk.green(shortDigest(entry.digest))
    : chalk.yellow("(no digest)");
  const age = formatLoadedAt(entry.loaded_at);
  const ageDisplay = age
    ? chalk.dim(`(${age})`)
    : chalk.yellow("(never loaded)");
  const err = entry.last_error
    ? chalk.red(`\n        last_error: ${entry.last_error}`)
    : "";
  return `${chalk.bold(source)} ${digest} ${ageDisplay}${err}`;
}

function shortDigest(digest: string): string {
  // Match the controller's `…/admission/util.rs` truncation length so
  // the operator can copy-paste between `inspect` output and a
  // controller log line.
  const colon = digest.indexOf(":");
  if (colon < 0) return digest;
  const prefix = digest.slice(0, colon + 1);
  const hex = digest.slice(colon + 1);
  return `${prefix}${hex.slice(0, 12)}…`;
}

function sourceLabel(path: string): string {
  if (!path) return "?";
  const trimmed = path.replace(/\/+$/, "");
  const last = trimmed.split("/").pop();
  return last || trimmed;
}

/// `loaded_at` ships as RFC 3339 (`format_rfc3339` in
/// `routes/internal.rs`). Render relative-to-now for the eyeballed
/// view; absolute timestamp is available via `--json`.
function formatLoadedAt(rfc3339: string): string | undefined {
  const t = Date.parse(rfc3339);
  if (!Number.isFinite(t) || t <= 0) return undefined;
  const deltaMs = Date.now() - t;
  if (deltaMs < 0) return "in the future";
  const s = Math.round(deltaMs / 1000);
  if (s < 60) return `${s}s ago`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.round(m / 60);
  if (h < 48) return `${h}h ago`;
  const d = Math.round(h / 24);
  return `${d}d ago`;
}

// Internal exports for unit tests — keep these surface-area-minimal.
export const __test__ = {
  renderEntry,
  shortDigest,
  formatLoadedAt,
  POLICY_KIND_LABEL,
};
