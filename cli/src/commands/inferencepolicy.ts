// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import chalk from "chalk";
import { readFile } from "node:fs/promises";
import {
  formatAge,
  parseKVPairs,
  parseSpecFile,
  stripUndefined,
} from "./crd-helpers.js";
import { runApply, runDelete, runGet, runList } from "./toolpolicy.js";

const KIND = "InferencePolicy";
const PLURAL = "inferencepolicies";

const SEVERITIES = ["Safe", "Low", "Medium", "High"] as const;
type Severity = (typeof SEVERITIES)[number];

export interface InferencePolicyApplyOptions {
  fromFile?: string;
  sandbox?: string;
  action?: string;
  sandboxLabel?: string[];
  tokenBudget?: string;
  monthlyTokens?: string;
  perRequestTokens?: string;
  model?: string;
  provider?: string;
  fallback?: string[];
  contentSafetySeverity?: string;
  requirePromptShields?: boolean;
  displayName?: string;
}

function parseUint(raw: string, flag: string): number {
  const n = Number(raw);
  if (!Number.isInteger(n) || n < 0) {
    throw new Error(`${flag} must be a non-negative integer (got '${raw}')`);
  }
  return n;
}

function parseProviderRef(raw: string): { provider: string; deployment: string } {
  // Format: provider:deployment
  const idx = raw.indexOf(":");
  if (idx <= 0 || idx === raw.length - 1) {
    throw new Error(`expected 'provider:deployment' but got '${raw}'`);
  }
  return { provider: raw.slice(0, idx).trim(), deployment: raw.slice(idx + 1).trim() };
}

export function buildInferencePolicySpecFromFlags(
  o: InferencePolicyApplyOptions,
): Record<string, unknown> {
  const sandboxMatchLabels = parseKVPairs(o.sandboxLabel);
  const appliesTo: Record<string, unknown> = {
    sandboxName: o.sandbox,
    action: o.action,
    sandboxMatchLabels:
      Object.keys(sandboxMatchLabels).length > 0 ? sandboxMatchLabels : undefined,
  };

  let tokenBudget: Record<string, unknown> | undefined;
  if (
    o.tokenBudget !== undefined ||
    o.monthlyTokens !== undefined ||
    o.perRequestTokens !== undefined
  ) {
    tokenBudget = {
      dailyTokens:
        o.tokenBudget !== undefined ? parseUint(o.tokenBudget, "--token-budget") : undefined,
      monthlyTokens:
        o.monthlyTokens !== undefined ? parseUint(o.monthlyTokens, "--monthly-tokens") : undefined,
      perRequestTokens:
        o.perRequestTokens !== undefined
          ? parseUint(o.perRequestTokens, "--per-request-tokens")
          : undefined,
    };
  }

  let modelPreference: Record<string, unknown> | undefined;
  if (o.model) {
    const provider = o.provider ?? "azure-openai";
    modelPreference = {
      primary: { provider, deployment: o.model },
      fallback: (o.fallback ?? []).map(parseProviderRef),
    };
  }

  let contentSafety: Record<string, unknown> | undefined;
  if (o.contentSafetySeverity || o.requirePromptShields) {
    if (
      o.contentSafetySeverity &&
      !SEVERITIES.includes(o.contentSafetySeverity as Severity)
    ) {
      throw new Error(
        `--content-safety-severity must be one of ${SEVERITIES.join("|")} (got '${o.contentSafetySeverity}')`,
      );
    }
    const sev = o.contentSafetySeverity;
    contentSafety = {
      hate: sev,
      selfHarm: sev,
      sexual: sev,
      violence: sev,
      requirePromptShields: o.requirePromptShields,
    };
  }

  return stripUndefined({
    appliesTo,
    tokenBudget,
    modelPreference,
    contentSafety,
    displayName: o.displayName,
  }) as Record<string, unknown>;
}

/**
 * Validate a flag-built spec. Mirrors the user-facing requirement: if you
 * don't provide a `--from-file`, you must specify *some* policy shaping —
 * a model, a token budget, or a content-safety severity. Empty policies
 * are useless and confuse `precedence` resolution.
 */
export function validateInferencePolicySpec(
  spec: Record<string, unknown>,
  fromFile: boolean,
): string[] {
  const errs: string[] = [];
  if (!spec.appliesTo || typeof spec.appliesTo !== "object") {
    errs.push("missing required spec.appliesTo");
  }
  if (!fromFile) {
    const hasShaping =
      spec.modelPreference !== undefined ||
      spec.tokenBudget !== undefined ||
      spec.contentSafety !== undefined;
    if (!hasShaping) {
      errs.push(
        "missing required spec.model — pass --model, --token-budget, or --content-safety-severity, or include in --from-file",
      );
    }
  }
  return errs;
}

export function summarizeInferencePolicyRow(
  item: Record<string, unknown>,
  now: Date = new Date(),
): string[] {
  const meta = (item.metadata ?? {}) as Record<string, unknown>;
  const spec = (item.spec ?? {}) as Record<string, unknown>;
  const status = (item.status ?? {}) as Record<string, unknown>;
  const at = (spec.appliesTo ?? {}) as Record<string, unknown>;
  const tb = (spec.tokenBudget ?? {}) as Record<string, unknown>;
  const mp = (spec.modelPreference ?? {}) as Record<string, unknown>;
  const primary = (mp.primary ?? {}) as Record<string, unknown>;
  const model = primary.deployment ? `${primary.provider ?? "?"}:${primary.deployment}` : "-";
  return [
    String(meta.name ?? "<unknown>"),
    String(at.sandboxName ?? "*"),
    String(model),
    String(tb.dailyTokens ?? "-"),
    formatAge(meta.creationTimestamp as string | undefined, now),
    String(status.phase ?? "-"),
  ];
}

export function inferencePolicyCommand(): Command {
  const cmd = new Command("inferencepolicy")
    .alias("ip")
    .description("Manage InferencePolicy CRs (token budgets, model preference, Content Safety floor)");

  cmd
    .command("apply")
    .description("Create or update an InferencePolicy")
    .argument("<name>", "InferencePolicy name (DNS-1123)")
    .option("-n, --namespace <ns>", "Namespace", "default")
    .option("--from-file <path>", "Read spec from a YAML/JSON file")
    .option("--sandbox <name>", "Restrict to a specific sandbox")
    .option("--action <kind>", "Inference action: chat|responses|image|embeddings|*")
    .option("--sandbox-label <kv>", "Sandbox match label key=value (repeatable)", appendOpt, [] as string[])
    .option("--token-budget <n>", "Daily token cap (input + output)")
    .option("--monthly-tokens <n>", "Monthly token cap")
    .option("--per-request-tokens <n>", "Per-request token cap")
    .option("--model <deployment>", "Primary model deployment name")
    .option("--provider <name>", "Provider tag for --model (default azure-openai)")
    .option("--fallback <provider:deployment>", "Fallback route (repeatable)", appendOpt, [] as string[])
    .option("--content-safety-severity <sev>", "Severity floor for all CS categories: Safe|Low|Medium|High")
    .option("--require-prompt-shields", "Require Prompt Shields annotations from upstream", false)
    .option("--display-name <s>", "Human-readable display name")
    .action(async (name: string, opts: InferencePolicyApplyOptions & { namespace: string }) => {
      const fromFile = !!opts.fromFile;
      await runApply(
        name,
        opts,
        opts.namespace,
        KIND,
        PLURAL,
        async () => {
          if (opts.fromFile) {
            const content = await readFile(opts.fromFile, "utf8");
            return parseSpecFile(content);
          }
          return buildInferencePolicySpecFromFlags(opts);
        },
        (s) => validateInferencePolicySpec(s, fromFile),
      );
    });

  cmd
    .command("get")
    .description("Show an InferencePolicy by name")
    .argument("<name>", "Name")
    .option("-n, --namespace <ns>", "Namespace", "default")
    .option("-o, --output <fmt>", "Output: pretty|yaml|json", "pretty")
    .action(async (name: string, opts: { namespace: string; output: string }) => {
      await runGet(name, opts.namespace, opts.output, PLURAL, (item) => {
        const spec = (item.spec ?? {}) as Record<string, unknown>;
        const status = (item.status ?? {}) as Record<string, unknown>;
        const mp = (spec.modelPreference ?? {}) as Record<string, unknown>;
        const tb = (spec.tokenBudget ?? {}) as Record<string, unknown>;
        const cs = (spec.contentSafety ?? {}) as Record<string, unknown>;
        console.log(chalk.bold(`\n  InferencePolicy/${name}\n`));
        console.log(`  AppliesTo:    ${formatBlock(spec.appliesTo)}`);
        console.log(`  Model:        ${formatBlock(mp.primary)}`);
        console.log(`  TokenBudget:  ${formatBlock(tb)}`);
        console.log(`  CS Floor:     ${formatBlock(cs)}`);
        console.log(`  Phase:        ${status.phase ?? "-"}\n`);
      });
    });

  cmd
    .command("list")
    .description("List InferencePolicies in a namespace")
    .option("-n, --namespace <ns>", "Namespace", "default")
    .action(async (opts: { namespace: string }) => {
      await runList(opts.namespace, PLURAL, ["NAME", "SANDBOX", "MODEL", "DAILY", "AGE", "STATUS"], summarizeInferencePolicyRow);
    });

  cmd
    .command("delete")
    .description("Delete an InferencePolicy")
    .argument("<name>", "Name")
    .option("-n, --namespace <ns>", "Namespace", "default")
    .option("--no-prompt", "Skip confirmation")
    .action(async (name: string, opts: { namespace: string; prompt: boolean }) => {
      await runDelete(name, opts.namespace, PLURAL, opts.prompt);
    });

  return cmd;
}

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

export const __test = {
  buildInferencePolicySpecFromFlags,
  validateInferencePolicySpec,
  summarizeInferencePolicyRow,
};
