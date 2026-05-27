// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `kars audit tail <sandbox>` — stream the durable JSONL audit
//! log produced by the router's `audit_jsonl::JsonlAuditWriter`
//! (Slice 4 DoD #4 + #7).
//!
//! The router writes one JSON line per `agentmesh::AuditEntry` to
//! `/var/log/kars/audit/{YYYY-MM-DD}.jsonl` inside the
//! `inference-router` container. This command shells into the pod
//! and runs `cat`/`tail` against today's file, optionally filtering
//! and pretty-printing the rows.
//!
//! No port-forward, no host process — matches the operational
//! pattern established by `kars inspect`. The router's local
//! file is the authoritative source; remote sinks (Slice 4c) will
//! mirror the same lines.

import { Command } from "commander";
import chalk from "chalk";
import { execa } from "execa";

/// Default audit-log directory inside the sandbox pod. Matches
/// `KARS_AUDIT_DIR` default in `inference-router/src/governance/mod.rs::open_jsonl_writer`.
export const DEFAULT_AUDIT_DIR = "/var/log/kars/audit";

/// Shape of one row written by `JsonlAuditWriter::write`.
/// Mirrors `audit_jsonl::AuditRow` byte-for-byte (additive: tests
/// gracefully ignore extra fields).
export interface AuditRow {
  sandbox: string;
  seq: number;
  ts: string;
  agent_id: string;
  action: string;
  decision: string;
  prev_hash: string;
  hash: string;
}

export interface AuditTailOptions {
  namespace?: string;
  lines?: string;
  follow?: boolean;
  decision?: string;
  agent?: string;
  action?: string;
  json?: boolean;
  date?: string;
  dir?: string;
}

export function auditCommand(): Command {
  const cmd = new Command("audit").description(
    "Inspect the durable JSONL audit log of a sandbox's router (Slice 4)"
  );

  cmd
    .command("tail <sandbox>")
    .description(
      "Stream the durable JSONL audit log of a sandbox's router"
    )
    .option(
      "-n, --namespace <ns>",
      "Sandbox pod namespace (default: 'kars-<sandbox>')"
    )
    .option(
      "-l, --lines <n>",
      "Number of trailing lines to show (default: 50, max: 10000)",
      "50"
    )
    .option("-f, --follow", "Keep streaming new audit rows as they arrive")
    .option(
      "--decision <state>",
      "Filter by decision (e.g. 'allowed', 'denied', 'flagged', 'sanitized')"
    )
    .option("--agent <id>", "Filter by agent_id (exact match)")
    .option(
      "--action <substring>",
      "Filter rows whose action contains this substring (case-sensitive)"
    )
    .option(
      "--date <YYYY-MM-DD>",
      "Read a specific date's file instead of today's (UTC)"
    )
    .option(
      "--dir <path>",
      `Override the in-pod audit directory (default: ${DEFAULT_AUDIT_DIR})`
    )
    .option("--json", "Emit each row as raw JSON instead of the pretty table")
    .action(async (sandbox: string, opts: AuditTailOptions) => {
      try {
        await runAuditTail(sandbox, opts);
      } catch (e: unknown) {
        const msg = e instanceof Error ? e.message : String(e);
        console.error(chalk.red(`\n  audit tail failed: ${msg}\n`));
        process.exitCode = 1;
      }
    });

  return cmd;
}

export async function runAuditTail(
  sandbox: string,
  opts: AuditTailOptions
): Promise<void> {
  const ns = opts.namespace ?? `kars-${sandbox}`;
  const dir = opts.dir ?? DEFAULT_AUDIT_DIR;
  const lines = parseLines(opts.lines);
  const dateKey = opts.date ?? todayUtcKey();
  validateDateKey(dateKey);
  const file = `${dir}/${dateKey}.jsonl`;

  // Build the in-pod command. We deliberately use `tail` (not `cat`)
  // even for the non-follow case so a huge log file doesn't blow up
  // the kubectl pipe — the router writes one ~300 byte JSON line per
  // audited request, and the operator-friendly default is "show me
  // the recent ones".
  const tailArgs = [`-n${lines}`];
  if (opts.follow) tailArgs.push("-F");
  // The router writes one line per entry, and entries are written
  // strictly in seq order, so trailing tail is a chronological tail.
  const script =
    `if [ ! -f ${shellQuote(file)} ]; then echo "AUDIT_FILE_MISSING ${shellQuote(file)}" >&2; exit 2; fi; ` +
    `exec tail ${tailArgs.join(" ")} ${shellQuote(file)}`;

  const child = execa(
    "kubectl",
    [
      "exec",
      ...(opts.follow ? [] : []),
      "-n",
      ns,
      `deploy/${sandbox}`,
      "-c",
      "inference-router",
      "--",
      "sh",
      "-c",
      script,
    ],
    {
      stdio: ["ignore", "pipe", "pipe"],
      reject: false,
      buffer: false,
    }
  );

  if (!child.stdout || !child.stderr) {
    throw new Error("kubectl exec did not yield a stdout/stderr pipe");
  }

  let printedHeader = false;
  const renderRow = (raw: string) => {
    if (!raw.trim()) return;
    const row = parseRow(raw);
    if (!row) {
      // Malformed line — surface but don't crash the stream.
      console.error(chalk.yellow(`  (skipping malformed audit line)`));
      return;
    }
    if (!matchesFilters(row, opts)) return;
    if (opts.json) {
      console.log(JSON.stringify(row));
      return;
    }
    if (!printedHeader) {
      console.log(renderHeader());
      printedHeader = true;
    }
    console.log(renderPrettyLine(row));
  };

  let buffer = "";
  child.stdout.setEncoding("utf8");
  child.stdout.on("data", (chunk: string) => {
    buffer += chunk;
    let idx: number;
    while ((idx = buffer.indexOf("\n")) >= 0) {
      const line = buffer.slice(0, idx);
      buffer = buffer.slice(idx + 1);
      renderRow(line);
    }
  });

  child.stderr.setEncoding("utf8");
  let stderrBuf = "";
  child.stderr.on("data", (chunk: string) => {
    stderrBuf += chunk;
  });

  const result = await child;
  if (buffer.trim()) renderRow(buffer);

  if (result.exitCode !== 0) {
    const msg = stderrBuf.trim() || `kubectl exec exited with ${result.exitCode}`;
    if (msg.includes("AUDIT_FILE_MISSING")) {
      throw new Error(
        `No audit log file at ${file} in pod 'deploy/${sandbox}' (ns '${ns}').\n` +
          `  Either nothing has been audited yet on that date or the router\n` +
          `  is running with KARS_AUDIT_DIR=disabled.`
      );
    }
    throw new Error(msg);
  }
}

/// Today's date key in UTC — matches the router's
/// `audit_jsonl::date_key_from_timestamp` which slices `entry.timestamp[..10]`.
/// We honour UTC so an operator on a US west-coast laptop sees the same
/// file name the router wrote at boot in eastern-time UTC.
export function todayUtcKey(now: Date = new Date()): string {
  const yyyy = now.getUTCFullYear().toString().padStart(4, "0");
  const mm = (now.getUTCMonth() + 1).toString().padStart(2, "0");
  const dd = now.getUTCDate().toString().padStart(2, "0");
  return `${yyyy}-${mm}-${dd}`;
}

const DATE_KEY_RE = /^\d{4}-\d{2}-\d{2}$/;

export function validateDateKey(key: string): void {
  if (!DATE_KEY_RE.test(key)) {
    throw new Error(
      `Invalid --date '${key}' — expected YYYY-MM-DD (matches router rotation key)`
    );
  }
}

export function parseLines(raw: string | undefined): number {
  const n = Number.parseInt(raw ?? "50", 10);
  if (!Number.isFinite(n) || n < 1) {
    throw new Error(`Invalid --lines '${raw}' — must be a positive integer`);
  }
  if (n > 10000) {
    throw new Error(`--lines ${n} exceeds the safe cap of 10000`);
  }
  return n;
}

export function parseRow(line: string): AuditRow | null {
  try {
    const parsed = JSON.parse(line) as Partial<AuditRow>;
    if (
      typeof parsed.sandbox !== "string" ||
      typeof parsed.seq !== "number" ||
      typeof parsed.ts !== "string" ||
      typeof parsed.agent_id !== "string" ||
      typeof parsed.action !== "string" ||
      typeof parsed.decision !== "string" ||
      typeof parsed.prev_hash !== "string" ||
      typeof parsed.hash !== "string"
    ) {
      return null;
    }
    return parsed as AuditRow;
  } catch {
    return null;
  }
}

export function matchesFilters(row: AuditRow, opts: AuditTailOptions): boolean {
  if (opts.decision && row.decision !== opts.decision) return false;
  if (opts.agent && row.agent_id !== opts.agent) return false;
  if (opts.action && !row.action.includes(opts.action)) return false;
  return true;
}

export function renderHeader(): string {
  return chalk.dim(
    [
      "seq".padEnd(6),
      "timestamp".padEnd(22),
      "decision".padEnd(12),
      "agent".padEnd(28),
      "action",
    ].join("  ")
  );
}

export function renderPrettyLine(row: AuditRow): string {
  const decisionColor = colorForDecision(row.decision);
  return [
    row.seq.toString().padEnd(6),
    row.ts.slice(0, 19).padEnd(22),
    decisionColor(row.decision.padEnd(12)),
    truncate(row.agent_id, 28).padEnd(28),
    truncate(row.action, 80),
  ].join("  ");
}

function colorForDecision(decision: string): (s: string) => string {
  switch (decision) {
    case "allowed":
    case "success":
      return chalk.green;
    case "denied":
    case "flagged":
    case "rejected":
      return chalk.red;
    case "sanitized":
      return chalk.yellow;
    default:
      return chalk.white;
  }
}

function truncate(s: string, n: number): string {
  if (s.length <= n) return s;
  if (n <= 1) return s.slice(0, n);
  return `${s.slice(0, n - 1)}…`;
}

/// Single-quote-shell-quote a path that may contain spaces. The
/// caller controls the input (a path under /var/log/kars/audit)
/// but we still defend against the user passing --dir with a quote.
function shellQuote(s: string): string {
  return `'${s.replace(/'/g, "'\\''")}'`;
}
