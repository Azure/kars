// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Slice 6.4 — `kars eval` operator surface for the KarsEval CRD.
//
// Replaces the legacy Foundry-Evals wrapper. KarsEval is now a
// policy-conformance runner driven by signed corpora (slice 6.1) +
// the conformance-runner image (slice 6.2) + the controller
// reconciler (slice 6.3). This CLI is the read/trigger surface
// operators reach for day-to-day:
//
//   kars eval list                       — list KarsEvals across the controller ns
//   kars eval show <name>                — print spec + last-run + drift status
//   kars eval run <name>                 — set `kars.azure.com/run-now=true`
//   kars eval diff <name>                — diff the two most recent runs in history
//
// All commands hit the apiserver directly via `kubectl`; no router
// admin token required (operator can still see the CR even when the
// router is unhealthy).

import { Command } from "commander";
import chalk from "chalk";

import { formatAge, formatTable } from "./crd-helpers.js";

const PLURAL = "karsevals";
const DEFAULT_NS = "kars-system";

interface KarsEvalCondition {
  type?: string;
  status?: string;
  reason?: string;
  message?: string;
  lastTransitionTime?: string;
}

interface EvalCaseResult {
  caseId?: string;
  scenario?: string;
  outcome?: string;
  failureKind?: string | null;
  detail?: string | null;
}

interface EvalResult {
  startedAt?: string;
  finishedAt?: string;
  totalCases?: number;
  passedCases?: number;
  failedCases?: number;
  drift?: boolean;
  corpusLabel?: string;
  jobName?: string;
  cases?: EvalCaseResult[];
}

interface KarsEvalStatus {
  phase?: string;
  observedGeneration?: number;
  conditions?: KarsEvalCondition[];
  lastRunAt?: string;
  lastResult?: EvalResult;
  history?: EvalResult[];
}

interface KarsEvalSpec {
  targetSandboxRef?: { name?: string };
  corpus?: {
    builtin?: string;
    bundleRef?: {
      registry?: string;
      repository?: string;
      digest?: string;
      artifactType?: string;
    };
  };
  schedule?: string;
  failSandboxOnDrift?: boolean;
  displayName?: string;
  runnerImage?: string;
  notifyWebhook?: { url?: string };
}

interface KarsEvalCR {
  metadata?: {
    name?: string;
    namespace?: string;
    creationTimestamp?: string;
    annotations?: Record<string, string>;
  };
  spec?: KarsEvalSpec;
  status?: KarsEvalStatus;
}

/** Shape one row of `kars eval list` output. Exported so tests
 *  can drive it without spawning `kubectl`. */
export function summarizeEvalRow(item: KarsEvalCR, now: Date = new Date()): string[] {
  const md = item.metadata ?? {};
  const spec = item.spec ?? {};
  const status = item.status ?? {};
  const last = status.lastResult;

  const name = md.name ?? "<unknown>";
  const target = spec.targetSandboxRef?.name ?? "—";
  const corpus = spec.corpus?.builtin
    ? `builtin:${spec.corpus.builtin}`
    : spec.corpus?.bundleRef?.digest
      ? `bundle:${spec.corpus.bundleRef.digest.slice(0, 19)}`
      : "—";
  const phase = status.phase ?? "Pending";
  const age = formatAge(md.creationTimestamp);
  const summary = last
    ? `${last.passedCases ?? 0}/${last.totalCases ?? 0}${last.drift ? " ⚠ drift" : ""}`
    : "—";

  return [name, target, corpus, phase, age, summary];
}

/** Pretty-print the show subcommand. Exported for tests. */
export function renderEvalShow(item: KarsEvalCR): string {
  const lines: string[] = [];
  const md = item.metadata ?? {};
  const spec = item.spec ?? {};
  const status = item.status ?? {};
  const last = status.lastResult;

  lines.push("");
  lines.push(chalk.bold(`  KarsEval/${md.name ?? "<unknown>"}`));
  lines.push(`    namespace:           ${md.namespace ?? DEFAULT_NS}`);
  lines.push(`    target sandbox:      ${spec.targetSandboxRef?.name ?? "—"}`);
  if (spec.corpus?.builtin) {
    lines.push(`    corpus:              builtin:${spec.corpus.builtin}`);
  } else if (spec.corpus?.bundleRef) {
    const br = spec.corpus.bundleRef;
    lines.push(`    corpus:              bundle ${br.registry}/${br.repository}@${br.digest ?? ""}`);
  }
  if (spec.schedule) {
    lines.push(`    schedule:            ${spec.schedule}`);
  } else {
    lines.push(`    schedule:            (on-demand only)`);
  }
  lines.push(`    failSandboxOnDrift:  ${spec.failSandboxOnDrift ? "true" : "false"}`);
  if (spec.notifyWebhook?.url) {
    lines.push(`    notifyWebhook:       ${spec.notifyWebhook.url}`);
  }

  lines.push("");
  lines.push(chalk.bold("  Status"));
  lines.push(`    phase:               ${status.phase ?? "Pending"}`);
  lines.push(`    observedGeneration:  ${status.observedGeneration ?? 0}`);
  if (status.lastRunAt) {
    lines.push(`    lastRunAt:           ${status.lastRunAt}`);
  }
  for (const c of status.conditions ?? []) {
    lines.push(`    ${(c.type ?? "?").padEnd(20)} ${c.status ?? "?"} (${c.reason ?? ""})`);
    if (c.message) lines.push(`      ${chalk.dim(c.message)}`);
  }

  if (last) {
    lines.push("");
    lines.push(chalk.bold("  Last run"));
    lines.push(`    corpus:              ${last.corpusLabel ?? "—"}`);
    lines.push(`    started:             ${last.startedAt ?? "—"}`);
    lines.push(`    finished:            ${last.finishedAt ?? "—"}`);
    lines.push(`    cases:               ${last.passedCases ?? 0}/${last.totalCases ?? 0} passed`);
    if (last.drift) lines.push(chalk.red(`    drift:               YES`));
    if (last.jobName) lines.push(`    job:                 ${last.jobName}`);
    const failed = (last.cases ?? []).filter(c => c.outcome === "Fail");
    if (failed.length > 0) {
      lines.push("");
      lines.push(chalk.bold("  Failures"));
      for (const f of failed.slice(0, 10)) {
        lines.push(`    • ${f.caseId ?? "?"} (${f.scenario ?? "?"}) — ${f.failureKind ?? "?"}`);
        if (f.detail) lines.push(`      ${chalk.dim(f.detail)}`);
      }
      if (failed.length > 10) {
        lines.push(chalk.dim(`    … and ${failed.length - 10} more`));
      }
    }
  }
  lines.push("");
  return lines.join("\n");
}

/** Render the diff between two runs. Exported for tests.
 *  Format: cases that changed outcome are highlighted; otherwise
 *  shows aggregate movement. */
export function renderEvalDiff(older: EvalResult, newer: EvalResult): string {
  const lines: string[] = [];
  lines.push("");
  lines.push(chalk.bold("  Run diff (older → newer)"));
  lines.push(`    started:   ${older.startedAt ?? "—"}  →  ${newer.startedAt ?? "—"}`);
  lines.push(
    `    passed:    ${older.passedCases ?? 0}/${older.totalCases ?? 0}  →  ${newer.passedCases ?? 0}/${newer.totalCases ?? 0}`,
  );
  if (older.drift !== newer.drift) {
    lines.push(
      `    drift:     ${older.drift ? "YES" : "no"}  →  ${newer.drift ? chalk.red("YES") : "no"}`,
    );
  }

  // Per-case diff: build maps keyed by caseId.
  const olderMap = new Map<string, EvalCaseResult>();
  for (const c of older.cases ?? []) {
    if (c.caseId) olderMap.set(c.caseId, c);
  }
  const newerMap = new Map<string, EvalCaseResult>();
  for (const c of newer.cases ?? []) {
    if (c.caseId) newerMap.set(c.caseId, c);
  }
  const allIds = new Set<string>([...olderMap.keys(), ...newerMap.keys()]);

  const regressions: EvalCaseResult[] = [];
  const fixes: EvalCaseResult[] = [];
  const newlyAdded: EvalCaseResult[] = [];
  const dropped: EvalCaseResult[] = [];

  for (const id of allIds) {
    const o = olderMap.get(id);
    const n = newerMap.get(id);
    if (o && !n) dropped.push(o);
    else if (!o && n) newlyAdded.push(n);
    else if (o && n) {
      if (o.outcome === "Pass" && n.outcome === "Fail") regressions.push(n);
      else if (o.outcome === "Fail" && n.outcome === "Pass") fixes.push(n);
    }
  }

  if (regressions.length > 0) {
    lines.push("");
    lines.push(chalk.red(chalk.bold(`  Regressions (${regressions.length})`)));
    for (const r of regressions) {
      lines.push(`    ✗ ${r.caseId ?? "?"} (${r.scenario ?? "?"}) — ${r.failureKind ?? "?"}`);
    }
  }
  if (fixes.length > 0) {
    lines.push("");
    lines.push(chalk.green(chalk.bold(`  Fixes (${fixes.length})`)));
    for (const f of fixes) {
      lines.push(`    ✓ ${f.caseId ?? "?"} (${f.scenario ?? "?"})`);
    }
  }
  if (newlyAdded.length > 0) {
    lines.push("");
    lines.push(chalk.dim(`  Added cases (${newlyAdded.length}): ${newlyAdded.map(c => c.caseId).join(", ")}`));
  }
  if (dropped.length > 0) {
    lines.push("");
    lines.push(chalk.dim(`  Dropped cases (${dropped.length}): ${dropped.map(c => c.caseId).join(", ")}`));
  }
  if (regressions.length === 0 && fixes.length === 0 && newlyAdded.length === 0 && dropped.length === 0) {
    lines.push("");
    lines.push(chalk.dim("  No per-case differences."));
  }
  lines.push("");
  return lines.join("\n");
}

async function listEvals(namespace: string, jsonOutput: boolean): Promise<void> {
  const { execa } = await import("execa");
  try {
    const { stdout } = await execa(
      "kubectl",
      ["get", PLURAL, "-n", namespace, "-o", "json"],
      { stdio: "pipe" },
    );
    const parsed = JSON.parse(stdout) as { items?: KarsEvalCR[] };
    const items = parsed.items ?? [];
    if (jsonOutput) {
      process.stdout.write(`${JSON.stringify(items, null, 2)}\n`);
      return;
    }
    if (items.length === 0) {
      console.log(chalk.dim(`  No KarsEvals found in namespace '${namespace}'.`));
      return;
    }
    const headers = ["NAME", "TARGET", "CORPUS", "PHASE", "AGE", "LAST"];
    const rows = items.map(it => ({ cells: summarizeEvalRow(it) }));
    console.log("\n" + formatTable(headers, rows) + "\n");
  } catch (e) {
    console.error(chalk.red(`\nError: ${e instanceof Error ? e.message : String(e)}\n`));
    process.exitCode = 1;
  }
}

async function getEval(name: string, namespace: string): Promise<KarsEvalCR> {
  const { execa } = await import("execa");
  const { stdout } = await execa(
    "kubectl",
    ["get", PLURAL, name, "-n", namespace, "-o", "json"],
    { stdio: "pipe" },
  );
  return JSON.parse(stdout) as KarsEvalCR;
}

async function showEval(name: string, namespace: string, jsonOutput: boolean): Promise<void> {
  try {
    const item = await getEval(name, namespace);
    if (jsonOutput) {
      process.stdout.write(`${JSON.stringify(item, null, 2)}\n`);
      return;
    }
    process.stdout.write(renderEvalShow(item));
  } catch (e) {
    console.error(chalk.red(`\nError: ${e instanceof Error ? e.message : String(e)}\n`));
    process.exitCode = 1;
  }
}

async function runEval(name: string, namespace: string): Promise<void> {
  const { execa } = await import("execa");
  try {
    // Strategic merge patch: set the run-now annotation. The controller
    // reconciler picks this up, spawns a one-shot Job, and clears the
    // annotation after Job creation (slice 6.3 reconciler).
    const patch = JSON.stringify({
      metadata: { annotations: { "kars.azure.com/run-now": "true" } },
    });
    await execa(
      "kubectl",
      ["annotate", PLURAL, name, "-n", namespace,
       "kars.azure.com/run-now=true", "--overwrite"],
      { stdio: "pipe" },
    );
    void patch; // patch object kept for documentation; we use `kubectl annotate` which is simpler
    console.log(chalk.green(`\n  ✓ Run triggered for KarsEval/${name}\n`));
    console.log(chalk.dim(`    Watch progress with: kars eval show ${name}`));
    console.log("");
  } catch (e) {
    console.error(chalk.red(`\nError triggering run: ${e instanceof Error ? e.message : String(e)}\n`));
    process.exitCode = 1;
  }
}

async function diffEval(name: string, namespace: string): Promise<void> {
  try {
    const item = await getEval(name, namespace);
    const history = item.status?.history ?? [];
    const last = item.status?.lastResult;

    // Walk: prefer (history[len-1], lastResult) when both present;
    // fall back to (history[len-2], history[len-1]) when only history
    // is populated.
    let older: EvalResult | undefined;
    let newer: EvalResult | undefined;
    if (last && history.length >= 1) {
      older = history[history.length - 1];
      newer = last;
    } else if (history.length >= 2) {
      older = history[history.length - 2];
      newer = history[history.length - 1];
    }

    if (!older || !newer) {
      console.log(chalk.dim(`  KarsEval/${name} has fewer than 2 runs — nothing to diff.`));
      process.exitCode = 0;
      return;
    }

    process.stdout.write(renderEvalDiff(older, newer));
  } catch (e) {
    console.error(chalk.red(`\nError: ${e instanceof Error ? e.message : String(e)}\n`));
    process.exitCode = 1;
  }
}

export function evalCommand(): Command {
  const cmd = new Command("eval");

  cmd.description("Manage and inspect KarsEval conformance runs (slice 6 — policy conformance corpus runner)");

  cmd
    .command("list")
    .description("List KarsEvals in the controller namespace")
    .option("-n, --namespace <ns>", "Controller namespace", DEFAULT_NS)
    .option("--json", "Emit JSON instead of a table", false)
    .action(async (opts: { namespace: string; json: boolean }) => {
      await listEvals(opts.namespace, opts.json);
    });

  cmd
    .command("show <name>")
    .description("Show spec + status for one KarsEval, including last-run results")
    .option("-n, --namespace <ns>", "Controller namespace", DEFAULT_NS)
    .option("--json", "Emit JSON instead of human-readable output", false)
    .action(async (name: string, opts: { namespace: string; json: boolean }) => {
      await showEval(name, opts.namespace, opts.json);
    });

  cmd
    .command("run <name>")
    .description("Trigger a one-shot run by annotating the CR with kars.azure.com/run-now=true")
    .option("-n, --namespace <ns>", "Controller namespace", DEFAULT_NS)
    .action(async (name: string, opts: { namespace: string }) => {
      await runEval(name, opts.namespace);
    });

  cmd
    .command("diff <name>")
    .description("Compare the two most recent runs in status.history")
    .option("-n, --namespace <ns>", "Controller namespace", DEFAULT_NS)
    .action(async (name: string, opts: { namespace: string }) => {
      await diffEval(name, opts.namespace);
    });

  return cmd;
}

export const __test = {
  summarizeEvalRow,
  renderEvalShow,
  renderEvalDiff,
};
