// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import chalk from "chalk";
import { readFile } from "node:fs/promises";
import {
  buildCR,
  formatAge,
  formatTable,
  parseKVPairs,
  parseSpecFile,
  stripUndefined,
  toYaml,
  validateName,
} from "./crd-helpers.js";

const KIND = "ToolPolicy";
const PLURAL = "toolpolicies";

export interface ToolPolicyApplyOptions {
  fromFile?: string;
  tool?: string;
  mcpServer?: string;
  sandboxLabel?: string[];
  rps?: string;
  burst?: string;
  window?: string;
  dailyCap?: string;
  monthlyCap?: string;
  perTransferCap?: string;
  counterparty?: string[];
  approvalMode?: string;
  approvalThreshold?: string;
  approvalChannel?: string;
  displayName?: string;
}

export function buildToolPolicySpecFromFlags(
  o: ToolPolicyApplyOptions,
): Record<string, unknown> {
  const sandboxMatchLabels = parseKVPairs(o.sandboxLabel);
  const appliesTo: Record<string, unknown> = {
    tool: o.tool,
    mcpServer: o.mcpServer,
    sandboxMatchLabels:
      Object.keys(sandboxMatchLabels).length > 0 ? sandboxMatchLabels : undefined,
  };

  let rateLimit: Record<string, unknown> | undefined;
  if (o.rps !== undefined || o.burst !== undefined || o.window !== undefined) {
    rateLimit = {
      rps: o.rps !== undefined ? parseUint(o.rps, "--rps") : undefined,
      burst: o.burst !== undefined ? parseUint(o.burst, "--burst") : undefined,
      window: o.window,
    };
  }

  let commerce: Record<string, unknown> | undefined;
  if (
    o.dailyCap ||
    o.monthlyCap ||
    o.perTransferCap ||
    (o.counterparty && o.counterparty.length > 0)
  ) {
    commerce = {
      dailyCap: o.dailyCap,
      monthlyCap: o.monthlyCap,
      perTransferCap: o.perTransferCap,
      counterpartyAllowlist: o.counterparty,
    };
  }

  let approval: Record<string, unknown> | undefined;
  if (o.approvalMode || o.approvalThreshold || o.approvalChannel) {
    if (o.approvalMode && !["never", "always", "aboveThreshold"].includes(o.approvalMode)) {
      throw new Error(
        `--approval-mode must be one of never|always|aboveThreshold (got '${o.approvalMode}')`,
      );
    }
    approval = {
      mode: o.approvalMode,
      threshold: o.approvalThreshold,
      channel: o.approvalChannel,
    };
  }

  return stripUndefined({
    appliesTo,
    rateLimit,
    commerce,
    approval,
    displayName: o.displayName,
  }) as Record<string, unknown>;
}

function parseUint(raw: string, flag: string): number {
  const n = Number(raw);
  if (!Number.isInteger(n) || n < 0) {
    throw new Error(`${flag} must be a non-negative integer (got '${raw}')`);
  }
  return n;
}

export function validateToolPolicySpec(spec: Record<string, unknown>): string[] {
  const errs: string[] = [];
  const at = spec.appliesTo as Record<string, unknown> | undefined;
  if (!at || typeof at !== "object") {
    errs.push("missing required spec.appliesTo — pass --tool / --mcp-server / --sandbox-label, or include in --from-file");
    return errs;
  }
  const hasSelector =
    (typeof at.tool === "string" && at.tool.length > 0) ||
    (typeof at.mcpServer === "string" && at.mcpServer.length > 0) ||
    (typeof at.sandboxMatchLabels === "object" &&
      at.sandboxMatchLabels !== null &&
      Object.keys(at.sandboxMatchLabels as object).length > 0);
  if (!hasSelector) {
    errs.push(
      "spec.appliesTo must specify at least one of: tool, mcpServer, sandboxMatchLabels",
    );
  }
  return errs;
}

export function summarizeToolPolicyRow(
  item: Record<string, unknown>,
  now: Date = new Date(),
): string[] {
  const meta = (item.metadata ?? {}) as Record<string, unknown>;
  const spec = (item.spec ?? {}) as Record<string, unknown>;
  const status = (item.status ?? {}) as Record<string, unknown>;
  const appliesTo = (spec.appliesTo ?? {}) as Record<string, unknown>;
  const commerce = (spec.commerce ?? {}) as Record<string, unknown>;
  return [
    String(meta.name ?? "<unknown>"),
    String(appliesTo.tool ?? "*"),
    String(commerce.dailyCap ?? "-"),
    formatAge(meta.creationTimestamp as string | undefined, now),
    String(status.phase ?? "-"),
  ];
}

export function toolPolicyCommand(): Command {
  const cmd = new Command("toolpolicy")
    .alias("tp")
    .description("Manage ToolPolicy CRs (per-tool gating, rate-limit, AP2 commerce caps)");

  cmd
    .command("apply")
    .description("Create or update a ToolPolicy")
    .argument("<name>", "ToolPolicy name (DNS-1123)")
    .option("-n, --namespace <ns>", "Namespace", "default")
    .option("--from-file <path>", "Read spec from a YAML/JSON file")
    .option("--tool <name>", "Tool name selector (use '*' for all)")
    .option("--mcp-server <name>", "Restrict to a specific MCP server")
    .option("--sandbox-label <kv>", "Sandbox match label key=value (repeatable)", appendOpt, [] as string[])
    .option("--rps <n>", "Rate limit: requests per second")
    .option("--burst <n>", "Rate limit: token-bucket burst")
    .option("--window <s>", "Rate limit window, e.g. '1m', '24h'")
    .option("--daily-cap <s>", "AP2 daily cap, e.g. 'USD 100.00'")
    .option("--monthly-cap <s>", "AP2 monthly cap")
    .option("--per-transfer-cap <s>", "AP2 per-transfer cap")
    .option("--counterparty <s>", "AP2 counterparty allow-list entry (repeatable)", appendOpt, [] as string[])
    .option("--approval-mode <mode>", "Approval mode: never|always|aboveThreshold")
    .option("--approval-threshold <s>", "Approval threshold value")
    .option("--approval-channel <s>", "Approval channel ref")
    .option("--display-name <s>", "Human-readable display name")
    .action(async (name: string, opts: ToolPolicyApplyOptions & { namespace: string }) => {
      await runApply(name, opts, opts.namespace, KIND, PLURAL, async () => {
        if (opts.fromFile) {
          const content = await readFile(opts.fromFile, "utf8");
          return parseSpecFile(content);
        }
        return buildToolPolicySpecFromFlags(opts);
      }, validateToolPolicySpec);
    });

  cmd
    .command("get")
    .description("Show a ToolPolicy by name")
    .argument("<name>", "Name")
    .option("-n, --namespace <ns>", "Namespace", "default")
    .option("-o, --output <fmt>", "Output: pretty|yaml|json", "pretty")
    .action(async (name: string, opts: { namespace: string; output: string }) => {
      await runGet(name, opts.namespace, opts.output, PLURAL, (item) => {
        const spec = (item.spec ?? {}) as Record<string, unknown>;
        const at = (spec.appliesTo ?? {}) as Record<string, unknown>;
        console.log(chalk.bold(`\n  ToolPolicy/${name}\n`));
        console.log(`  Tool:        ${at.tool ?? "*"}`);
        console.log(`  MCPServer:   ${at.mcpServer ?? "-"}`);
        console.log(`  RateLimit:   ${formatBlock(spec.rateLimit)}`);
        console.log(`  Commerce:    ${formatBlock(spec.commerce)}`);
        console.log(`  Approval:    ${formatBlock(spec.approval)}`);
        const status = (item.status ?? {}) as Record<string, unknown>;
        console.log(`  Phase:       ${status.phase ?? "-"}\n`);
      });
    });

  cmd
    .command("list")
    .description("List ToolPolicies in a namespace")
    .option("-n, --namespace <ns>", "Namespace", "default")
    .action(async (opts: { namespace: string }) => {
      await runList(opts.namespace, PLURAL, ["NAME", "TOOL", "DAILYCAP", "AGE", "STATUS"], summarizeToolPolicyRow);
    });

  cmd
    .command("delete")
    .description("Delete a ToolPolicy by name")
    .argument("<name>", "Name")
    .option("-n, --namespace <ns>", "Namespace", "default")
    .option("--no-prompt", "Skip confirmation")
    .action(async (name: string, opts: { namespace: string; prompt: boolean }) => {
      await runDelete(name, opts.namespace, PLURAL, opts.prompt);
    });

  return cmd;
}

// --- shared runtime helpers (apply/get/list/delete) ----------------------

function appendOpt(value: string, prev: string[]): string[] {
  return [...(prev ?? []), value];
}

function formatBlock(v: unknown): string {
  if (!v || typeof v !== "object") return "-";
  const entries = Object.entries(v as Record<string, unknown>).filter(
    ([, x]) => x !== undefined && x !== null,
  );
  if (entries.length === 0) return "-";
  return entries.map(([k, x]) => `${k}=${JSON.stringify(x)}`).join(", ");
}

export async function runApply(
  name: string,
  _opts: unknown,
  namespace: string,
  kind: string,
  plural: string,
  buildSpec: () => Promise<Record<string, unknown>>,
  validate: (s: Record<string, unknown>) => string[],
): Promise<void> {
  const { execa } = await import("execa");
  const nameErrs = validateName(name);
  if (nameErrs.length > 0) {
    console.error(chalk.red(`\nError: ${nameErrs.join("; ")}\n`));
    process.exitCode = 1;
    return;
  }
  let spec: Record<string, unknown>;
  try {
    spec = await buildSpec();
  } catch (e) {
    const m = e instanceof Error ? e.message : String(e);
    console.error(chalk.red(`\nError: ${m}\n`));
    process.exitCode = 1;
    return;
  }
  const errs = validate(spec);
  if (errs.length > 0) {
    console.error(chalk.red(`\nError: ${errs.join("; ")}\n`));
    process.exitCode = 1;
    return;
  }
  const cr = buildCR(kind, name, namespace, spec);
  const yaml = toYaml(cr);
  try {
    await execa("kubectl", ["apply", "-n", namespace, "-f", "-"], {
      input: yaml,
      stdio: ["pipe", "pipe", "pipe"],
    });
    console.log(chalk.green(`\n  ✓ ${kind}/${name} applied (namespace: ${namespace})\n`));
  } catch (e) {
    const m = e instanceof Error ? e.message : String(e);
    console.error(chalk.red(`\nError applying ${kind}: ${m}\n`));
    process.exitCode = 1;
  }
  void plural; // silence unused
}

export async function runGet(
  name: string,
  namespace: string,
  output: string,
  plural: string,
  pretty: (item: Record<string, unknown>) => void,
): Promise<void> {
  const { execa } = await import("execa");
  try {
    const args = ["get", plural, name, "-n", namespace, "-o"];
    if (output === "yaml") {
      const { stdout } = await execa("kubectl", [...args, "yaml"], { stdio: "pipe" });
      process.stdout.write(stdout + "\n");
      return;
    }
    if (output === "json") {
      const { stdout } = await execa("kubectl", [...args, "json"], { stdio: "pipe" });
      process.stdout.write(stdout + "\n");
      return;
    }
    const { stdout } = await execa("kubectl", [...args, "json"], { stdio: "pipe" });
    pretty(JSON.parse(stdout));
  } catch (e) {
    const m = e instanceof Error ? e.message : String(e);
    console.error(chalk.red(`\nError: ${m}\n`));
    process.exitCode = 1;
  }
}

export async function runList(
  namespace: string,
  plural: string,
  headers: string[],
  rowOf: (item: Record<string, unknown>, now?: Date) => string[],
): Promise<void> {
  const { execa } = await import("execa");
  try {
    const { stdout } = await execa(
      "kubectl",
      ["get", plural, "-n", namespace, "-o", "json"],
      { stdio: "pipe" },
    );
    const list = JSON.parse(stdout);
    const items = (list.items ?? []) as Record<string, unknown>[];
    if (items.length === 0) {
      console.log(chalk.dim(`  No ${plural} found in namespace '${namespace}'.`));
      return;
    }
    const rows = items.map((it) => ({ cells: rowOf(it) }));
    console.log("\n" + formatTable(headers, rows) + "\n");
  } catch (e) {
    const m = e instanceof Error ? e.message : String(e);
    console.error(chalk.red(`\nError: ${m}\n`));
    process.exitCode = 1;
  }
}

export async function runDelete(
  name: string,
  namespace: string,
  plural: string,
  prompt: boolean,
): Promise<void> {
  const { execa } = await import("execa");
  if (prompt) {
    // Best-effort interactive prompt; non-TTY callers should pass --no-prompt.
    const inquirer = (await import("inquirer")).default;
    const { confirm } = await inquirer.prompt([
      {
        type: "confirm",
        name: "confirm",
        message: `Delete ${plural}/${name} in namespace '${namespace}'?`,
        default: false,
      },
    ]);
    if (!confirm) {
      console.log(chalk.dim("  Cancelled."));
      return;
    }
  }
  try {
    await execa("kubectl", ["delete", plural, name, "-n", namespace], { stdio: "pipe" });
    console.log(chalk.green(`\n  ✓ ${plural}/${name} deleted (namespace: ${namespace})\n`));
  } catch (e) {
    const m = e instanceof Error ? e.message : String(e);
    console.error(chalk.red(`\nError deleting: ${m}\n`));
    process.exitCode = 1;
  }
}

export const __test = {
  buildToolPolicySpecFromFlags,
  validateToolPolicySpec,
  summarizeToolPolicyRow,
};
