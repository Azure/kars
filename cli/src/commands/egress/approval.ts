// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `kars egress allow-extra | approvals | revoke` — the CLI surface
//! for the `EgressApproval` CRD (Slice 5e). Closes the "operator wants
//! to grant a hostname for the next 15 minutes without resigning the
//! baseline bundle" loop from
//! `docs/internal/crd-well-oiled-machine/slice-5e-egress-approvals.md`.
//!
//! Producer side (controller) and consumer side (router) shipped in
//! Slice 5e.1 + 5e.2 (PRs #308, #309). This file is pure CLI ergonomics
//! on top of `kubectl apply / get / delete` against the CRD — no
//! /internal router endpoint involved.
//!
//! Validation here mirrors the controller-side validation in
//! `controller/src/egress_approval_reconciler.rs::validate_*` so the
//! operator gets immediate feedback instead of waiting for a `Pending`
//! status with a terminal reason. Re-validating CEL rules client-side
//! is a deliberate UX choice (kube admission also re-validates).

import { Command } from "commander";
import chalk from "chalk";
import { readFile } from "node:fs/promises";
import {
  buildCR,
  formatAge,
  formatTable,
  parseSpecFile,
  stripUndefined,
  toYaml,
  validateName,
} from "../crd-helpers.js";

const KIND = "EgressApproval";
const PLURAL = "egressapprovals";

// Mirrors `controller/src/egress_approval_reconciler.rs`:
//   - hard ceiling: 7 days
//   - reason length: 1..=512 bytes, no ASCII control bytes (except \t/\n/\r)
//   - hosts: 1..=16 entries
//   - ticket: when set, 1..=128 chars
const HARD_TTL_CEILING_SECONDS = 7 * 24 * 3600;
const REASON_MAX_BYTES = 512;
const TICKET_MAX_BYTES = 128;
const HOSTS_MAX = 16;

/** Result of parsing one `--host` flag. */
export interface ParsedHost {
  host: string;
  port?: number;
}

/**
 * Parse a `--host` value of either `example.com` or `example.com:443`
 * into the same `EndpointConfig` shape the controller expects. Port
 * defaults to unset (router treats absent = match all ports for the
 * host) — preserving baseline semantics.
 */
export function parseHostEndpoint(raw: string): ParsedHost {
  const trimmed = raw.trim();
  if (!trimmed) {
    throw new Error("--host value is empty");
  }
  // Reject schemes and paths early — those are user mistakes.
  if (trimmed.includes("://") || trimmed.includes("/")) {
    throw new Error(
      `--host '${raw}' must be a bare hostname (optionally :port), not a URL`,
    );
  }
  const idx = trimmed.lastIndexOf(":");
  if (idx < 0) {
    return { host: trimmed };
  }
  const host = trimmed.slice(0, idx);
  const portStr = trimmed.slice(idx + 1);
  if (!host) {
    throw new Error(`--host '${raw}' is missing the hostname before ':'`);
  }
  const port = Number(portStr);
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error(
      `--host '${raw}' has an invalid port '${portStr}' (expected 1..=65535)`,
    );
  }
  return { host, port };
}

/**
 * Parse an ISO 8601 duration into a positive integer of seconds. Mirrors
 * `parse_iso8601_duration_secs` in the controller reconciler. Accepts
 * the subset the controller supports: `PnD`, `PTnH`, `PTnM`, `PTnS`,
 * and combinations like `PT1H30M`. Rejects weeks, months, years, and
 * malformed orderings (e.g. `P1H` with H before T, `PT1D` with D
 * after T).
 */
export function parseIsoDurationSecs(raw: string): number {
  const m = raw.trim();
  if (!m.startsWith("P")) throw new Error("must start with 'P'");
  if (m.length < 2) throw new Error("empty after 'P'");

  let i = 1;
  let inTime = false;
  let days = 0;
  let hours = 0;
  let minutes = 0;
  let seconds = 0;
  let seenAny = false;

  while (i < m.length) {
    if (m[i] === "T") {
      if (inTime) throw new Error("duplicate 'T' designator");
      inTime = true;
      i += 1;
      continue;
    }
    // Read digits
    let j = i;
    while (j < m.length && m[j] >= "0" && m[j] <= "9") j += 1;
    if (j === i) {
      throw new Error(`expected digit at position ${i} in '${raw}'`);
    }
    const n = Number(m.slice(i, j));
    if (!Number.isFinite(n)) {
      throw new Error(`overflow parsing number in '${raw}'`);
    }
    const designator = m[j];
    if (designator === undefined) {
      throw new Error(`trailing digits without designator in '${raw}'`);
    }
    if (!inTime) {
      // Date part — only D supported. W, Y, M reject.
      if (designator !== "D") {
        throw new Error(
          `unsupported date designator '${designator}' (only D before T is supported)`,
        );
      }
      days = n;
    } else {
      // Time part — H, M, S only.
      if (designator === "H") hours = n;
      else if (designator === "M") minutes = n;
      else if (designator === "S") seconds = n;
      else {
        throw new Error(
          `unsupported time designator '${designator}' (expected H, M, or S)`,
        );
      }
    }
    seenAny = true;
    i = j + 1;
  }

  if (!seenAny) throw new Error("no components in duration");
  const total = days * 86400 + hours * 3600 + minutes * 60 + seconds;
  if (total <= 0) throw new Error("duration must be > 0");
  return total;
}

/** Reject empty / oversize / ASCII-control-byte reasons. */
export function validateReasonText(reason: string): string | null {
  if (!reason) return "must be non-empty";
  const bytes = new TextEncoder().encode(reason).length;
  if (bytes === 0) return "must be non-empty";
  if (bytes > REASON_MAX_BYTES) {
    return `must be ≤ ${REASON_MAX_BYTES} bytes (got ${bytes})`;
  }
  for (let i = 0; i < reason.length; i += 1) {
    const code = reason.charCodeAt(i);
    // Reject control bytes 0x00..0x1F except TAB (0x09), LF (0x0A), CR (0x0D);
    // and reject DEL (0x7F).
    if (
      (code < 0x20 && code !== 0x09 && code !== 0x0a && code !== 0x0d) ||
      code === 0x7f
    ) {
      return `contains ASCII control byte 0x${code.toString(16).padStart(2, "0")} at position ${i}`;
    }
  }
  return null;
}

export interface AllowExtraOptions {
  namespace: string;
  host?: string[];
  ttl?: string;
  reason?: string;
  ticket?: string;
  fromFile?: string;
}

/**
 * Build the EgressApproval `spec` from CLI flags. Pure — no I/O. Throws
 * on flag errors so the caller can surface a single chalk-red message.
 */
export function buildEgressApprovalSpecFromFlags(
  sandbox: string,
  o: AllowExtraOptions,
): Record<string, unknown> {
  if (!sandbox) {
    throw new Error("sandbox name is required");
  }
  const hosts = (o.host ?? []).map(parseHostEndpoint);
  if (hosts.length === 0) {
    throw new Error("at least one --host is required");
  }
  if (hosts.length > HOSTS_MAX) {
    throw new Error(`too many --host entries (got ${hosts.length}, max ${HOSTS_MAX})`);
  }
  if (!o.ttl) {
    throw new Error("--ttl is required (e.g. --ttl PT15M)");
  }
  const ttlSecs = parseIsoDurationSecs(o.ttl);
  if (ttlSecs > HARD_TTL_CEILING_SECONDS) {
    throw new Error(
      `--ttl exceeds the hard ceiling of 7 days (${HARD_TTL_CEILING_SECONDS}s, got ${ttlSecs}s)`,
    );
  }
  if (!o.reason) {
    throw new Error("--reason is required (audit-grade text, 1..=512 bytes)");
  }
  const reasonErr = validateReasonText(o.reason);
  if (reasonErr) {
    throw new Error(`--reason ${reasonErr}`);
  }
  if (o.ticket !== undefined) {
    const tbytes = new TextEncoder().encode(o.ticket).length;
    if (tbytes === 0) throw new Error("--ticket must be non-empty when set");
    if (tbytes > TICKET_MAX_BYTES) {
      throw new Error(`--ticket must be ≤ ${TICKET_MAX_BYTES} bytes (got ${tbytes})`);
    }
  }

  return stripUndefined({
    sandbox,
    hosts: hosts.map((h) => stripUndefined({ host: h.host, port: h.port })),
    reason: o.reason,
    ticket: o.ticket,
    ttl: o.ttl,
  }) as Record<string, unknown>;
}

/** Generate a deterministic-ish CR name based on sandbox + a short timestamp. */
export function deriveApprovalName(sandbox: string, now: Date = new Date()): string {
  // 2026-05-14T12:30:45.123Z -> 20260514-123045z
  // Drop millis, keep Z, lowercase, replace separators.
  const iso = now.toISOString();
  const noMs = iso.replace(/\.\d+Z$/, "Z");
  const compact = noMs.replace(/[-:]/g, "").toLowerCase().replace("t", "-");
  return `${sandbox}-extra-${compact}`;
}

/**
 * Render one row for the `kars egress approvals` table.
 * Columns: NAME / HOSTS / TTL / EXPIRES / PHASE / AGE.
 */
export function summarizeApprovalRow(
  item: Record<string, unknown>,
  now: Date = new Date(),
): string[] {
  const meta = (item.metadata ?? {}) as Record<string, unknown>;
  const spec = (item.spec ?? {}) as Record<string, unknown>;
  const status = (item.status ?? {}) as Record<string, unknown>;
  const hosts = Array.isArray(spec.hosts) ? spec.hosts : [];
  const expiresAt = (status.expiresAt as string | undefined) ?? "";
  let expiresCol = "-";
  if (expiresAt) {
    const t = Date.parse(expiresAt);
    if (!Number.isNaN(t)) {
      const remaining = Math.floor((t - now.getTime()) / 1000);
      expiresCol = remaining > 0 ? `in ${humanizeSecs(remaining)}` : "expired";
    }
  }
  return [
    String(meta.name ?? "<unknown>"),
    String(hosts.length),
    String(spec.ttl ?? "-"),
    expiresCol,
    String(status.phase ?? "-"),
    formatAge(meta.creationTimestamp as string | undefined, now),
  ];
}

function humanizeSecs(secs: number): string {
  if (secs < 60) return `${secs}s`;
  const m = Math.floor(secs / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 48) return `${h}h`;
  const d = Math.floor(h / 24);
  return `${d}d`;
}

// ---------------------------------------------------------------------
// kubectl wrappers — execa-based, mirror toolpolicy.runApply pattern.
// Kept inline (rather than reused from toolpolicy.ts) because the
// EgressApproval flow is slightly different: name is operator-supplied
// optional, sandbox is positional, output is shaped for human eyes.
// ---------------------------------------------------------------------

async function applyApproval(
  name: string,
  namespace: string,
  spec: Record<string, unknown>,
): Promise<void> {
  const { execa } = await import("execa");
  const nameErrs = validateName(name);
  if (nameErrs.length > 0) {
    console.error(chalk.red(`\nError: ${nameErrs.join("; ")}\n`));
    process.exitCode = 1;
    return;
  }
  const cr = buildCR(KIND, name, namespace, spec);
  const yaml = toYaml(cr);
  try {
    await execa("kubectl", ["apply", "-n", namespace, "-f", "-"], {
      input: yaml,
      stdio: ["pipe", "pipe", "pipe"],
    });
    const hostCount = Array.isArray(spec.hosts) ? (spec.hosts as unknown[]).length : 0;
    console.log(
      chalk.green(
        `\n  ✓ EgressApproval/${name} applied (namespace: ${namespace}, hosts: ${hostCount}, ttl: ${spec.ttl})\n`,
      ),
    );
    console.log(chalk.dim(`    Watch status: kubectl get egressapproval/${name} -n ${namespace} -w\n`));
  } catch (e) {
    const m = e instanceof Error ? e.message : String(e);
    console.error(chalk.red(`\nError applying EgressApproval: ${m}\n`));
    process.exitCode = 1;
  }
}

async function listApprovalsForSandbox(
  sandbox: string,
  namespace: string,
): Promise<void> {
  const { execa } = await import("execa");
  try {
    const { stdout } = await execa(
      "kubectl",
      ["get", PLURAL, "-n", namespace, "-o", "json"],
      { stdio: "pipe" },
    );
    const list = JSON.parse(stdout);
    const allItems = (list.items ?? []) as Record<string, unknown>[];
    // Server-side label-selector would be nicer; for now filter
    // client-side by spec.sandbox since we don't (yet) label the CR
    // with the sandbox name on apply.
    const items = allItems.filter((it) => {
      const sp = (it.spec ?? {}) as Record<string, unknown>;
      return sp.sandbox === sandbox;
    });
    if (items.length === 0) {
      console.log(
        chalk.dim(
          `  No EgressApprovals for sandbox '${sandbox}' in namespace '${namespace}'.`,
        ),
      );
      return;
    }
    const rows = items.map((it) => ({ cells: summarizeApprovalRow(it) }));
    console.log(
      "\n" +
        formatTable(["NAME", "HOSTS", "TTL", "EXPIRES", "PHASE", "AGE"], rows) +
        "\n",
    );
  } catch (e) {
    const m = e instanceof Error ? e.message : String(e);
    console.error(chalk.red(`\nError listing EgressApprovals: ${m}\n`));
    process.exitCode = 1;
  }
}

async function deleteApproval(
  name: string,
  namespace: string,
  prompt: boolean,
): Promise<void> {
  const { execa } = await import("execa");
  if (prompt) {
    const inquirer = (await import("inquirer")).default;
    const { confirm } = await inquirer.prompt([
      {
        type: "confirm",
        name: "confirm",
        message: `Revoke EgressApproval/${name} in namespace '${namespace}'? The router will fall back to the baseline allowlist for this sandbox.`,
        default: false,
      },
    ]);
    if (!confirm) {
      console.log(chalk.dim("  Cancelled."));
      return;
    }
  }
  try {
    await execa("kubectl", ["delete", PLURAL, name, "-n", namespace], { stdio: "pipe" });
    console.log(
      chalk.green(
        `\n  ✓ EgressApproval/${name} deleted (namespace: ${namespace}); reconciler will drop the mount key and stamp phase=Expired.\n`,
      ),
    );
  } catch (e) {
    const m = e instanceof Error ? e.message : String(e);
    console.error(chalk.red(`\nError deleting EgressApproval: ${m}\n`));
    process.exitCode = 1;
  }
}

// ---------------------------------------------------------------------
// Commander wiring.
// ---------------------------------------------------------------------

export function allowExtraCommand(): Command {
  const cmd = new Command("allow-extra")
    .description(
      "Grant additional egress hosts for one sandbox on top of the signed baseline (TTL-scoped, audit-logged)",
    )
    .argument("<sandbox>", "Target KarsSandbox name (must exist in --namespace)")
    .option("-n, --namespace <ns>", "Namespace", "default")
    .option(
      "--host <host[:port]>",
      "Host to grant (repeatable, 1..=16). Port optional (1..=65535).",
      appendOpt,
      [] as string[],
    )
    .option(
      "--ttl <iso8601>",
      "Time-to-live as ISO 8601 duration (e.g. PT15M, PT4H, P1D). Hard ceiling 7d.",
    )
    .option(
      "--reason <text>",
      "Audit-grade reason text (1..=512 bytes, no ASCII control bytes)",
    )
    .option(
      "--ticket <id>",
      "Optional incident/ticket reference surfaced in audit (≤ 128 bytes)",
    )
    .option(
      "--name <approval-name>",
      "Override the generated approval CR name (DNS-1123)",
    )
    .option(
      "--from-file <path>",
      "Read the EgressApproval spec from YAML/JSON instead of building from flags",
    )
    .action(
      async (
        sandbox: string,
        opts: AllowExtraOptions & { name?: string },
      ) => {
        let spec: Record<string, unknown>;
        let name: string;
        try {
          if (opts.fromFile) {
            const content = await readFile(opts.fromFile, "utf8");
            spec = parseSpecFile(content);
            // Sanity-check that file targets the right sandbox unless
            // the operator explicitly overrode it via positional arg.
            if (spec.sandbox && spec.sandbox !== sandbox) {
              throw new Error(
                `--from-file spec.sandbox='${spec.sandbox}' does not match positional sandbox='${sandbox}'`,
              );
            }
            spec = { ...spec, sandbox };
          } else {
            spec = buildEgressApprovalSpecFromFlags(sandbox, opts);
          }
          name = opts.name ?? deriveApprovalName(sandbox);
        } catch (e) {
          const m = e instanceof Error ? e.message : String(e);
          console.error(chalk.red(`\nError: ${m}\n`));
          process.exitCode = 1;
          return;
        }
        await applyApproval(name, opts.namespace, spec);
      },
    );
  return cmd;
}

export function approvalsCommand(): Command {
  return new Command("approvals")
    .description("List EgressApprovals targeting a sandbox")
    .argument("<sandbox>", "KarsSandbox name to filter by")
    .option("-n, --namespace <ns>", "Namespace", "default")
    .action(async (sandbox: string, opts: { namespace: string }) => {
      await listApprovalsForSandbox(sandbox, opts.namespace);
    });
}

export function revokeCommand(): Command {
  return new Command("revoke")
    .description("Revoke (delete) an EgressApproval by name — the router falls back to the baseline allowlist")
    .argument("<name>", "EgressApproval name")
    .option("-n, --namespace <ns>", "Namespace", "default")
    .option("--no-prompt", "Skip the interactive confirmation")
    .action(
      async (
        name: string,
        opts: { namespace: string; prompt: boolean },
      ) => {
        await deleteApproval(name, opts.namespace, opts.prompt);
      },
    );
}

function appendOpt(value: string, prev: string[]): string[] {
  return [...(prev ?? []), value];
}

export const __test = {
  parseHostEndpoint,
  parseIsoDurationSecs,
  validateReasonText,
  buildEgressApprovalSpecFromFlags,
  deriveApprovalName,
  summarizeApprovalRow,
  HARD_TTL_CEILING_SECONDS,
  REASON_MAX_BYTES,
  HOSTS_MAX,
};
