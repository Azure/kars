// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { Execute } from "./deployment-target.js";

export interface Metadata {
  name: string; namespace?: string; uid: string; resourceVersion: string;
  annotations?: Record<string, string>;
  ownerReferences?: Array<{ apiVersion: string; kind: string; name: string; uid: string; controller?: boolean; blockOwnerDeletion?: boolean }>;
  deletionTimestamp?: string;
}
export interface SourceRef { name: string; uid: string }
export interface Sandbox {
  metadata: Metadata;
  spec: { credentialsRef?: SourceRef | null; [key: string]: unknown };
  status?: { conditions?: Array<{ type: string; status: string; reason: string; message: string }> };
}
export interface Secret {
  apiVersion?: string; kind?: string; type?: string;
  metadata: Metadata; data?: Record<string, string>;
}
export const SOURCE_KEYS = [
  "TELEGRAM_BOT_TOKEN", "TELEGRAM_ALLOW_FROM", "SLACK_BOT_TOKEN", "DISCORD_BOT_TOKEN",
  "WHATSAPP_ENABLED", "BRAVE_API_KEY", "TAVILY_API_KEY", "EXA_API_KEY", "FIRECRAWL_API_KEY",
  "PERPLEXITY_API_KEY",
] as const;
export const SOURCE = {
  prefix: "kars-credential-source-",
  purpose: "kars.azure.com/credential-purpose",
  target: "kars.azure.com/credential-target",
  intent: "kars.azure.com/credential-binding-intent",
  sandboxUid: "kars.azure.com/credential-sandbox-uid",
  workspace: "kars.azure.com/credential-workspace",
  namespaceUid: "kars.azure.com/credential-namespace-uid",
} as const;
export const FLAG_ENV: Record<string, string> = {
  telegramToken: "TELEGRAM_BOT_TOKEN", telegramAllowFrom: "TELEGRAM_ALLOW_FROM",
  slackToken: "SLACK_BOT_TOKEN", discordToken: "DISCORD_BOT_TOKEN",
  braveApiKey: "BRAVE_API_KEY", tavilyApiKey: "TAVILY_API_KEY", exaApiKey: "EXA_API_KEY",
  firecrawlApiKey: "FIRECRAWL_API_KEY", perplexityApiKey: "PERPLEXITY_API_KEY", openaiApiKey: "OPENAI_API_KEY",
};

export function targetName(name: string, workspace: string): string {
  for (const value of [name, workspace]) {
    if (value.length > 63 || !/^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/.test(value)) {
      throw new Error("Credential target/workspace must be Kubernetes DNS labels");
    }
  }
  return `${SOURCE.prefix}${name}`;
}

export function identity(meta: Metadata): void {
  if (!meta?.name || !meta.uid || !meta.resourceVersion || meta.deletionTimestamp) {
    throw new Error("Credential API identity is incomplete or terminating");
  }
}

export async function run(
  execute: Execute, stage: string, args: string[], input?: object,
): Promise<string> {
  try {
    const result = await execute("kubectl", args, {
      stdio: "pipe", ...(input ? { input: JSON.stringify(input) } : {}),
    });
    return String(result.stdout);
  } catch (error) {
    // execa/admission errors can contain the entire Secret request. Never
    // attach the original error, stdout, stderr, or command input as a cause.
    const code = (error as { exitCode?: unknown }).exitCode;
    throw new Error(`${stage} failed (kubectl exit ${typeof code === "number" ? code : "unknown"})`);
  }
}

export function parse<T>(text: string): T {
  try { return JSON.parse(text) as T; }
  catch { throw new Error("Credential API returned invalid JSON"); }
}

export async function get<T extends { metadata: Metadata }>(
  execute: Execute, kind: string, name: string, namespace?: string,
): Promise<T | undefined> {
  const text = await run(execute, "Read credential resource", [
    "get", kind, name, ...(namespace ? ["-n", namespace] : []), "--ignore-not-found", "-o", "json",
  ]);
  if (!text.trim()) return undefined;
  const value = parse<T>(text);
  identity(value.metadata);
  if (value.metadata.name !== name || (namespace && value.metadata.namespace !== namespace)) {
    throw new Error("Credential API returned a different resource identity");
  }
  return value;
}

export function decode(secret?: Secret): Record<string, string> {
  const values: Record<string, string> = Object.create(null);
  for (const [key, value] of Object.entries(secret?.data ?? {})) {
    try {
      if (typeof value !== "string") throw new Error();
      const buffer = Buffer.from(value, "base64");
      if (buffer.toString("base64") !== value) throw new Error();
      values[key] = new TextDecoder("utf-8", { fatal: true }).decode(buffer);
    } catch { throw new Error("Credential Secret has invalid encoded environment data"); }
  }
  return values;
}

export function validateValues(values: Record<string, string>): void {
  let size = 0;
  for (const [key, value] of Object.entries(values)) {
    if (!(SOURCE_KEYS as readonly string[]).includes(key) || value.includes("\0")) {
      throw new Error("Source mode accepts only supported agent channel/search credentials, not provider/control-plane or arbitrary environment keys");
    }
    size += Buffer.byteLength(value);
  }
  if (size > 131_072) throw new Error("Credential source exceeds 128 KiB");
}

export function validateSource(secret: Secret, name: string, workspace: string, sandbox?: Sandbox): void {
  identity(secret.metadata);
  const meta = secret.metadata;
  const annotations = meta.annotations ?? {};
  if (meta.name !== targetName(name, workspace) || meta.namespace !== workspace || secret.type !== "Opaque"
      || annotations[SOURCE.purpose] !== "agent-source-v1" || annotations[SOURCE.target] !== name
      || annotations[SOURCE.workspace] !== workspace || annotations[SOURCE.intent] !== "explicit-reference-v1") {
    throw new Error("Existing source has incompatible purpose, type, or target; no takeover");
  }
  const refs = meta.ownerReferences ?? [];
  if (refs.length && (!sandbox || refs.length !== 1 || refs[0].apiVersion !== "kars.azure.com/v1alpha1"
      || refs[0].kind !== "KarsSandbox" || refs[0].name !== name || refs[0].uid !== sandbox.metadata.uid
      || refs[0].controller !== true || refs[0].blockOwnerDeletion !== false)) {
    throw new Error("Source belongs to a different Sandbox incarnation");
  }
  if (annotations[SOURCE.sandboxUid] !== undefined && annotations[SOURCE.sandboxUid] !== sandbox?.metadata.uid) {
    throw new Error("Source Sandbox UID binding differs; no automatic reuse");
  }
  if (annotations[SOURCE.namespaceUid] !== undefined
      && annotations[SOURCE.namespaceUid] !== sandbox?.metadata.annotations?.["kars.azure.com/namespace-uid"]) {
    throw new Error("Source runtime namespace binding differs");
  }
}

export function updatesFromFlags(options: Record<string, unknown>): Record<string, string> {
  return Object.fromEntries(Object.entries(FLAG_ENV)
    .filter(([flag]) => typeof options[flag] === "string" && options[flag] !== "")
    .map(([flag, env]) => [env, options[flag] as string]));
}

export function removedKeys(value?: string): string[] {
  return (value ?? "").split(",").map(key => key.trim()).filter(Boolean).map(key => {
    const flag = key.replace(/-([a-z])/g, (_, letter: string) => letter.toUpperCase());
    const env = FLAG_ENV[flag] ?? key;
    if (!/^[A-Z_][A-Z0-9_]*$/.test(env)) throw new Error("Credential removal requires a flag name or environment key");
    return env;
  });
}
