// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge Inc 4 — `kars approval` CLI subcommand.
//
// The steering primitive on the command line: list the human decisions a task
// fleet is waiting on, and approve / deny them. Works on a plain kars cluster
// (no Bridge) — a KarsApproval is a first-class kars CRD.
//
//   kars approval list [-n ns] [--task <name>] [--pending]
//   kars approval approve <name> [-n ns] [--by <who>] [--reason <text>]
//   kars approval deny    <name> [-n ns] [--by <who>] [--reason <text>]
//   kars approval show    <name> [-n ns]
//
// approve/deny patch spec.decision; the controller is the sole writer of
// status and drives the terminal transition + records the decision immutably.

import { Command } from "commander";
import chalk from "chalk";
import { userInfo } from "node:os";

interface ApprovalCr {
  metadata?: { name?: string; namespace?: string };
  spec?: {
    taskRef?: { name?: string };
    action?: { kind?: string; summary?: string; detail?: string; requestedTier?: number };
    ttl?: string;
    decision?: { verdict?: string; decider?: string; reason?: string };
  };
  status?: {
    phase?: string;
    decider?: string;
    decidedAt?: string;
    expiresAt?: string;
    boundEnvelopeDigest?: string;
  };
}

async function kubectlJson(args: string[]): Promise<unknown | null> {
  const { execa } = await import("execa");
  try {
    const { stdout } = await execa("kubectl", [...args, "-o", "json"], { stdio: "pipe" });
    return JSON.parse(stdout);
  } catch {
    return null;
  }
}

function phaseBadge(phase: string | undefined): string {
  switch (phase) {
    case "Approved":
      return chalk.green("Approved");
    case "Denied":
      return chalk.red("Denied");
    case "Pending":
      return chalk.yellow("Pending");
    case "Expired":
      return chalk.gray("Expired");
    case "Stale":
      return chalk.magenta("Stale");
    default:
      return phase ?? "—";
  }
}

function defaultDecider(by?: string): string {
  if (by && by.trim()) return by.trim();
  try {
    return userInfo().username || "unknown";
  } catch {
    return "unknown";
  }
}

/** Patch spec.decision via a strategic-merge patch. */
async function decide(
  name: string,
  namespace: string,
  verdict: "approve" | "deny",
  decider: string,
  reason: string | undefined,
): Promise<boolean> {
  const { execa } = await import("execa");
  const decision: Record<string, string> = { verdict, decider };
  if (reason && reason.trim()) decision.reason = reason.trim();
  const patch = JSON.stringify({ spec: { decision } });
  try {
    await execa(
      "kubectl",
      ["patch", "karsapproval", name, "-n", namespace, "--type", "merge", "-p", patch],
      { stdio: "pipe" },
    );
    return true;
  } catch (e) {
    process.stderr.write(chalk.red(`✗ failed to ${verdict} '${name}': ${(e as Error).message}\n`));
    return false;
  }
}

function formatList(items: ApprovalCr[]): string {
  if (items.length === 0) return chalk.dim("  No approvals.\n");
  const lines: string[] = [""];
  for (const a of items) {
    const name = a.metadata?.name ?? "?";
    const task = a.spec?.taskRef?.name ?? "?";
    const kind = a.spec?.action?.kind ?? "custom";
    const summary = a.spec?.action?.summary ?? "";
    lines.push(`  ${phaseBadge(a.status?.phase).padEnd(18)} ${chalk.bold(name)}`);
    lines.push(`      ${chalk.dim("task")} ${task}   ${chalk.dim("action")} ${kind}`);
    if (summary) lines.push(`      ${summary}`);
    if (a.status?.decider) {
      lines.push(`      ${chalk.dim("decided by")} ${a.status.decider}${a.status.decidedAt ? ` ${chalk.dim("at")} ${a.status.decidedAt}` : ""}`);
    } else if (a.status?.expiresAt) {
      lines.push(`      ${chalk.dim("expires")} ${a.status.expiresAt}`);
    }
    lines.push("");
  }
  return lines.join("\n");
}

export function approvalCommand(): Command {
  const cmd = new Command("approval");
  cmd.description(
    "Steer the fleet: list, approve, and deny the human decisions (HITL " +
      "approvals) a KarsTask is waiting on.",
  );

  cmd
    .command("list")
    .description("List approvals in a namespace.")
    .option("-n, --namespace <ns>", "Namespace", "kars-system")
    .option("--task <name>", "Only approvals gating this task")
    .option("--pending", "Only undecided (Pending) approvals")
    .option("--format <fmt>", "Output format: 'human' (default) or 'json'", "human")
    .action(
      async (options: { namespace: string; task?: string; pending?: boolean; format: string }) => {
        const list = (await kubectlJson([
          "get",
          "karsapproval",
          "-n",
          options.namespace,
        ])) as { items?: ApprovalCr[] } | null;
        let items = list?.items ?? [];
        if (options.task) items = items.filter((a) => a.spec?.taskRef?.name === options.task);
        if (options.pending) items = items.filter((a) => a.status?.phase === "Pending");
        if (options.format === "json") {
          console.log(JSON.stringify(items, null, 2));
        } else {
          console.log(formatList(items));
        }
      },
    );

  cmd
    .command("approve")
    .description("Approve an approval the fleet is waiting on.")
    .argument("<name>", "KarsApproval name")
    .option("-n, --namespace <ns>", "Namespace", "kars-system")
    .option("--by <who>", "Decider identity (defaults to your OS username)")
    .option("--reason <text>", "Justification recorded in the receipt")
    .action(async (name: string, options: { namespace: string; by?: string; reason?: string }) => {
      const decider = defaultDecider(options.by);
      const ok = await decide(name, options.namespace, "approve", decider, options.reason);
      if (!ok) process.exit(1);
      console.log(chalk.green(`✓ approved ${name} (as ${decider})`));
      console.log(chalk.dim("  The controller will record the decision and update the receipt."));
    });

  cmd
    .command("deny")
    .description("Deny an approval the fleet is waiting on.")
    .argument("<name>", "KarsApproval name")
    .option("-n, --namespace <ns>", "Namespace", "kars-system")
    .option("--by <who>", "Decider identity (defaults to your OS username)")
    .option("--reason <text>", "Justification recorded in the receipt")
    .action(async (name: string, options: { namespace: string; by?: string; reason?: string }) => {
      const decider = defaultDecider(options.by);
      const ok = await decide(name, options.namespace, "deny", decider, options.reason);
      if (!ok) process.exit(1);
      console.log(chalk.yellow(`✓ denied ${name} (as ${decider})`));
    });

  cmd
    .command("show")
    .description("Print the raw KarsApproval CR.")
    .argument("<name>", "KarsApproval name")
    .option("-n, --namespace <ns>", "Namespace", "kars-system")
    .action(async (name: string, options: { namespace: string }) => {
      const cr = (await kubectlJson([
        "get",
        "karsapproval",
        name,
        "-n",
        options.namespace,
      ])) as ApprovalCr | null;
      if (!cr) {
        process.stderr.write(chalk.red(`✗ approval '${name}' not found in '${options.namespace}'.\n`));
        process.exit(4);
        return;
      }
      console.log(JSON.stringify(cr, null, 2));
    });

  return cmd;
}

export const __test = { defaultDecider, formatList };
