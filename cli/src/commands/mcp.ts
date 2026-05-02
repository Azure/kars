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

const KIND = "McpServer";
const PLURAL = "mcpservers";

export interface McpApplyOptions {
  fromFile?: string;
  url?: string;
  productionMode?: boolean;
  oauthIssuer?: string;
  oauthAudience?: string;
  oauthResource?: string;
  scope?: string[];
  allowedTool?: string[];
  allowedSandboxLabel?: string[];
  displayName?: string;
}

export function buildMcpSpecFromFlags(o: McpApplyOptions): Record<string, unknown> {
  const allowedSandboxes = parseKVPairs(o.allowedSandboxLabel);
  const allowedSandboxesObj =
    Object.keys(allowedSandboxes).length > 0
      ? { matchLabels: allowedSandboxes }
      : undefined;

  let oauth: Record<string, unknown> | undefined;
  if (o.oauthIssuer || o.oauthAudience || o.oauthResource) {
    oauth = {
      issuer: o.oauthIssuer,
      audience: o.oauthAudience,
      resource: o.oauthResource,
    };
  }

  return stripUndefined({
    url: o.url,
    productionMode: o.productionMode ? true : undefined,
    oauth,
    scopes: o.scope && o.scope.length > 0 ? o.scope : undefined,
    allowedTools: o.allowedTool && o.allowedTool.length > 0 ? o.allowedTool : undefined,
    allowedSandboxes: allowedSandboxesObj,
    displayName: o.displayName,
  }) as Record<string, unknown>;
}

export function validateMcpSpec(spec: Record<string, unknown>): string[] {
  const errs: string[] = [];
  const url = spec.url as string | undefined;
  if (!url || typeof url !== "string" || url.length === 0) {
    errs.push("missing required spec.url — pass --url or include in --from-file");
  } else {
    if (!/^https?:\/\//.test(url)) {
      errs.push(`spec.url must start with http:// or https:// (got '${url}')`);
    }
    if (spec.productionMode === true && !url.startsWith("https://")) {
      errs.push("productionMode requires spec.url to begin with https://");
    }
  }
  if (spec.productionMode === true) {
    const oauth = spec.oauth as Record<string, unknown> | undefined;
    if (!oauth || typeof oauth.issuer !== "string" || oauth.issuer.length === 0) {
      errs.push("productionMode requires spec.oauth.issuer — pass --oauth-issuer");
    }
  }
  return errs;
}

export function summarizeMcpRow(
  item: Record<string, unknown>,
  now: Date = new Date(),
): string[] {
  const meta = (item.metadata ?? {}) as Record<string, unknown>;
  const spec = (item.spec ?? {}) as Record<string, unknown>;
  const status = (item.status ?? {}) as Record<string, unknown>;
  return [
    String(meta.name ?? "<unknown>"),
    String(spec.url ?? "-"),
    String(spec.productionMode === true ? "yes" : "no"),
    formatAge(meta.creationTimestamp as string | undefined, now),
    String(status.phase ?? "-"),
  ];
}

export function mcpCommand(): Command {
  const cmd = new Command("mcp")
    .description("Manage McpServer CRs (MCP 2026 servers reachable from sandboxes)");

  cmd
    .command("apply")
    .description("Create or update an McpServer")
    .argument("<name>", "McpServer name (DNS-1123)")
    .option("-n, --namespace <ns>", "Namespace", "default")
    .option("--from-file <path>", "Read spec from a YAML/JSON file")
    .option("--url <url>", "Server endpoint URL (https:// in production mode)")
    .option("--production-mode", "Require OAuth 2.1 + HTTPS", false)
    .option("--oauth-issuer <url>", "OAuth issuer URL")
    .option("--oauth-audience <s>", "OAuth audience claim")
    .option("--oauth-resource <s>", "OAuth resource indicator")
    .option("--scope <s>", "OAuth scope (repeatable)", appendOpt, [] as string[])
    .option("--allowed-tool <s>", "Allowed tool name (repeatable; use '*' for any)", appendOpt, [] as string[])
    .option("--allowed-sandbox-label <kv>", "Sandbox match label key=value (repeatable)", appendOpt, [] as string[])
    .option("--display-name <s>", "Human-readable display name")
    .action(async (name: string, opts: McpApplyOptions & { namespace: string }) => {
      await runApply(name, opts, opts.namespace, KIND, PLURAL, async () => {
        if (opts.fromFile) {
          const content = await readFile(opts.fromFile, "utf8");
          return parseSpecFile(content);
        }
        return buildMcpSpecFromFlags(opts);
      }, validateMcpSpec);
    });

  cmd
    .command("get")
    .description("Show an McpServer by name")
    .argument("<name>", "Name")
    .option("-n, --namespace <ns>", "Namespace", "default")
    .option("-o, --output <fmt>", "Output: pretty|yaml|json", "pretty")
    .action(async (name: string, opts: { namespace: string; output: string }) => {
      await runGet(name, opts.namespace, opts.output, PLURAL, (item) => {
        const spec = (item.spec ?? {}) as Record<string, unknown>;
        const status = (item.status ?? {}) as Record<string, unknown>;
        console.log(chalk.bold(`\n  McpServer/${name}\n`));
        console.log(`  URL:          ${spec.url ?? "-"}`);
        console.log(`  Production:   ${spec.productionMode === true ? "yes" : "no"}`);
        console.log(`  OAuth:        ${formatBlock(spec.oauth)}`);
        const tools = (spec.allowedTools ?? []) as string[];
        console.log(`  AllowedTools: ${tools.length === 0 ? "(none)" : tools.join(", ")}`);
        console.log(`  Phase:        ${status.phase ?? "-"}\n`);
      });
    });

  cmd
    .command("list")
    .description("List McpServers in a namespace")
    .option("-n, --namespace <ns>", "Namespace", "default")
    .action(async (opts: { namespace: string }) => {
      await runList(opts.namespace, PLURAL, ["NAME", "URL", "PROD", "AGE", "STATUS"], summarizeMcpRow);
    });

  cmd
    .command("delete")
    .description("Delete an McpServer")
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
  buildMcpSpecFromFlags,
  validateMcpSpec,
  summarizeMcpRow,
};
