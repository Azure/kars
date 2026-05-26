// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * `kars memory` — manage `KarsMemory` CRs.
 *
 * A `KarsMemory` binds a `KarsSandbox` to a Foundry Memory Store
 * (storeName + scope) with optional retention and a delete-on-sandbox-
 * delete finalizer. CRD shape is the source of truth: see
 * `deploy/helm/kars/templates/crd-karsmemory.yaml` (kind=KarsMemory,
 * plural=karsmemories, group=kars.azure.com/v1alpha1).
 *
 * Mirrors the structure of `mcp.ts` and `inferencepolicy.ts` so the CLI
 * surface stays uniform.
 */

import { Command } from "commander";
import chalk from "chalk";
import { readFile } from "node:fs/promises";
import {
  formatAge,
  parseSpecFile,
  stripUndefined,
} from "./crd-helpers.js";
import { runApply, runDelete, runGet, runList } from "./toolpolicy.js";

const KIND = "KarsMemory";
const PLURAL = "karsmemories";

export interface MemoryApplyOptions {
  fromFile?: string;
  sandbox?: string;
  store?: string;
  scope?: string;
  retentionDays?: number;
  displayName?: string;
  noDeleteOnSandboxDelete?: boolean;
}

export function buildMemorySpecFromFlags(o: MemoryApplyOptions): Record<string, unknown> {
  const sandboxRef = o.sandbox ? { name: o.sandbox } : undefined;
  return stripUndefined({
    sandboxRef,
    storeName: o.store,
    scope: o.scope,
    retentionDays: o.retentionDays,
    displayName: o.displayName,
    // Default in CRD is true; only emit when the operator explicitly opts out.
    deleteOnSandboxDelete: o.noDeleteOnSandboxDelete ? false : undefined,
  }) as Record<string, unknown>;
}

const DNS_LABEL_RE = /^[a-z0-9]([-a-z0-9]*[a-z0-9])?$/;

export function validateMemorySpec(spec: Record<string, unknown>): string[] {
  const errs: string[] = [];

  const sandboxRef = spec.sandboxRef as { name?: unknown } | undefined;
  if (!sandboxRef || typeof sandboxRef.name !== "string" || sandboxRef.name.length === 0) {
    errs.push("missing required spec.sandboxRef.name — pass --sandbox or include in --from-file");
  } else if (sandboxRef.name.length > 253) {
    errs.push("spec.sandboxRef.name must be 1–253 characters");
  }

  const store = spec.storeName;
  if (typeof store !== "string" || store.length === 0) {
    errs.push("missing required spec.storeName — pass --store");
  } else if (store.length > 63 || !DNS_LABEL_RE.test(store)) {
    errs.push(`spec.storeName must be a DNS-label (1–63 chars, lowercase alphanumeric + dashes): got '${store}'`);
  }

  const scope = spec.scope;
  if (typeof scope !== "string" || scope.length === 0) {
    errs.push("missing required spec.scope — pass --scope");
  } else if (scope.length > 256) {
    errs.push("spec.scope must be 1–256 characters");
  }

  const ret = spec.retentionDays;
  if (ret !== undefined) {
    if (typeof ret !== "number" || !Number.isInteger(ret) || ret <= 0) {
      errs.push("spec.retentionDays must be a positive integer (use --retention-days >0; omit to disable retention sweep)");
    }
  }

  return errs;
}

export function summarizeMemoryRow(
  item: Record<string, unknown>,
  now: Date = new Date(),
): string[] {
  const meta = (item.metadata ?? {}) as Record<string, unknown>;
  const spec = (item.spec ?? {}) as Record<string, unknown>;
  const status = (item.status ?? {}) as Record<string, unknown>;
  const sandboxRef = spec.sandboxRef as { name?: string } | undefined;
  const ret = spec.retentionDays;
  return [
    String(meta.name ?? "<unknown>"),
    String(sandboxRef?.name ?? "-"),
    String(spec.storeName ?? "-"),
    String(spec.scope ?? "-"),
    typeof ret === "number" ? `${ret}d` : "-",
    formatAge(meta.creationTimestamp as string | undefined, now),
    String(status.phase ?? "-"),
  ];
}

export function memoryCommand(): Command {
  const cmd = new Command("memory")
    .description("Manage KarsMemory CRs (Foundry Memory Store bindings for sandboxes)");

  cmd
    .command("apply")
    .description("Create or update a KarsMemory binding")
    .argument("<name>", "KarsMemory name (DNS-1123)")
    .option("-n, --namespace <ns>", "Namespace (use 'kars-<sandbox>')", "default")
    .option("--from-file <path>", "Read spec from a YAML/JSON file")
    .option("--sandbox <name>", "Sandbox to bind (spec.sandboxRef.name)")
    .option("--store <name>", "Foundry Memory Store name (DNS-label)")
    .option("--scope <key>", "Scope key under which this sandbox reads/writes (e.g. agent:my-agent)")
    .option("--retention-days <n>", "Retention floor in days (delete_scope sweep, >0)", (v) => parseInt(v, 10))
    .option("--display-name <s>", "Human-readable display label")
    .option("--no-delete-on-sandbox-delete", "Keep store contents when the sandbox is deleted (default: cleanup)")
    .action(async (name: string, opts: MemoryApplyOptions & { namespace: string }) => {
      await runApply(name, opts, opts.namespace, KIND, PLURAL, async () => {
        if (opts.fromFile) {
          const content = await readFile(opts.fromFile, "utf8");
          return parseSpecFile(content);
        }
        return buildMemorySpecFromFlags(opts);
      }, validateMemorySpec);
    });

  cmd
    .command("get")
    .description("Show a KarsMemory by name")
    .argument("<name>", "Name")
    .option("-n, --namespace <ns>", "Namespace", "default")
    .option("-o, --output <fmt>", "Output: pretty|yaml|json", "pretty")
    .action(async (name: string, opts: { namespace: string; output: string }) => {
      await runGet(name, opts.namespace, opts.output, PLURAL, (item) => {
        const spec = (item.spec ?? {}) as Record<string, unknown>;
        const status = (item.status ?? {}) as Record<string, unknown>;
        const sandboxRef = spec.sandboxRef as { name?: string } | undefined;
        console.log(chalk.bold(`\n  KarsMemory/${name}\n`));
        console.log(`  Sandbox:        ${sandboxRef?.name ?? "-"}`);
        console.log(`  Store:          ${spec.storeName ?? "-"}`);
        console.log(`  Scope:          ${spec.scope ?? "-"}`);
        const ret = spec.retentionDays;
        console.log(`  Retention:      ${typeof ret === "number" ? `${ret}d` : "-"}`);
        console.log(`  CleanupOnDel:   ${spec.deleteOnSandboxDelete === false ? "no" : "yes (default)"}`);
        console.log(`  DisplayName:    ${spec.displayName ?? "-"}`);
        console.log(`  Phase:          ${status.phase ?? "-"}\n`);
      });
    });

  cmd
    .command("list")
    .description("List KarsMemory bindings in a namespace")
    .option("-n, --namespace <ns>", "Namespace", "default")
    .action(async (opts: { namespace: string }) => {
      await runList(
        opts.namespace,
        PLURAL,
        ["NAME", "SANDBOX", "STORE", "SCOPE", "RETENTION", "AGE", "STATUS"],
        summarizeMemoryRow,
      );
    });

  cmd
    .command("delete")
    .description("Delete a KarsMemory binding")
    .argument("<name>", "Name")
    .option("-n, --namespace <ns>", "Namespace", "default")
    .option("--no-prompt", "Skip confirmation")
    .action(async (name: string, opts: { namespace: string; prompt: boolean }) => {
      await runDelete(name, opts.namespace, PLURAL, opts.prompt);
    });

  return cmd;
}

export const __test = {
  buildMemorySpecFromFlags,
  validateMemorySpec,
  summarizeMemoryRow,
};
