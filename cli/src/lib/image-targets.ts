// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { RuntimeKind } from "../runtime.js";
import type { Execute } from "./deployment-target.js";

/** The complete runtime image mapping formerly embedded in upgrade.ts, paired
 * with the existing controller environment and RuntimeSpec variant names. */
export const RUNTIME_IMAGE_TARGETS: ReadonlyArray<{
  name: string; repo: string; valueKey: string; env: string;
  kind: RuntimeKind; variant: string; language?: string;
}> = [
  { name: "runtime-openai-agents", repo: "kars-runtime-openai-agents", valueKey: "runtimes.openaiAgents.image", env: "OPENAI_AGENTS_RUNTIME_IMAGE", kind: "OpenAIAgents", variant: "openaiAgents" },
  { name: "runtime-maf-python", repo: "kars-runtime-maf-python", valueKey: "runtimes.mafPython.image", env: "MAF_RUNTIME_IMAGE", kind: "MicrosoftAgentFramework", variant: "microsoftAgentFramework", language: "python" },
  { name: "runtime-anthropic", repo: "kars-runtime-anthropic", valueKey: "runtimes.anthropic.image", env: "ANTHROPIC_RUNTIME_IMAGE", kind: "Anthropic", variant: "anthropic" },
  { name: "runtime-langgraph", repo: "kars-runtime-langgraph", valueKey: "runtimes.langgraph.image", env: "LANGGRAPH_RUNTIME_IMAGE", kind: "LangGraph", variant: "langGraph", language: "python" },
  { name: "runtime-langgraph-ts", repo: "kars-runtime-langgraph-ts", valueKey: "runtimes.langgraphTs.image", env: "LANGGRAPH_TS_RUNTIME_IMAGE", kind: "LangGraph", variant: "langGraph", language: "typescript" },
  { name: "runtime-pydantic-ai", repo: "kars-runtime-pydantic-ai", valueKey: "runtimes.pydanticAi.image", env: "PYDANTIC_AI_RUNTIME_IMAGE", kind: "PydanticAi", variant: "pydanticAi" },
  { name: "runtime-hermes", repo: "kars-runtime-hermes", valueKey: "runtimes.hermes.image", env: "HERMES_RUNTIME_IMAGE", kind: "Hermes", variant: "hermes" },
];

export interface PushedImage { name: string; image: string }
export const PUSH_COMPONENTS = ["controller", "router", "sandbox", "sandbox-base", "relay", "registry", ...RUNTIME_IMAGE_TARGETS.map(item => item.name)];

export function splitImage(image: string): { repository: string; tag: string } {
  const withoutDigest = image.split("@")[0];
  const index = withoutDigest.lastIndexOf(":");
  if (index <= withoutDigest.lastIndexOf("/") || index === withoutDigest.length - 1
    || /[\s,=]/.test(image) || (image.includes("@") && !/@sha256:[a-f0-9]{64}$/.test(image))) {
    throw new Error(`A valid repository:tag artifact is required: ${image}`);
  }
  return { repository: image.slice(0, index), tag: image.slice(index + 1) };
}

export function runtimeTarget(name: string) {
  return RUNTIME_IMAGE_TARGETS.find(item => item.name === name);
}

export function dockerPushDigest(output: string): string | undefined {
  const digests = new Set([...output.matchAll(/\bdigest:\s*(sha256:[a-f0-9]{64})\b/g)].map(match => match[1]));
  return digests.size === 1 ? [...digests][0] : undefined;
}

export function controllerEnv(name: string): string | undefined {
  return name === "router" ? "INFERENCE_ROUTER_IMAGE"
    : name === "sandbox" ? "SANDBOX_IMAGE" : runtimeTarget(name)?.env;
}

export function coreImageValues(images: PushedImage[]): Record<string, string> {
  const values: Record<string, string> = {};
  for (const { name, image } of images) {
    const runtime = runtimeTarget(name);
    if (runtime) values[runtime.valueKey] = image;
    else if (["controller", "router", "sandbox"].includes(name)) {
      const prefix = name === "router" ? "inferenceRouter" : name;
      const { repository, tag } = splitImage(image);
      values[`${prefix}.image.repository`] = repository;
      values[`${prefix}.image.tag`] = tag;
      values[`${prefix}.image.pullPolicy`] = "Always";
    }
  }
  return values;
}

export function imageValueArgs(values: Record<string, string>): string[] {
  return Object.entries(values).flatMap(([key, value]) => ["--set-string", `${key}=${value}`]);
}

/** Preserve the selected tag for readability, but bind it to the pushed ACR
 * manifest. Even an older IfNotPresent workload cannot reuse stale :latest. */
export async function resolvePushedArtifacts(execute: Execute, images: PushedImage[]): Promise<PushedImage[]> {
  const resolved: PushedImage[] = [];
  for (const item of images) {
    splitImage(item.image);
    if (item.image.includes("@")) { resolved.push(item); continue; }
    const slash = item.image.indexOf("/");
    const host = item.image.slice(0, slash);
    if (!/^[a-z0-9]+\.azurecr\.io$/i.test(host)) throw new Error("Pushed artifacts must identify their target ACR");
    const { stdout } = await execute("az", [
      "acr", "repository", "show", "--name", host.slice(0, -".azurecr.io".length),
      "--image", item.image.slice(slash + 1), "--query", "digest", "--output", "tsv",
    ], { stdio: "pipe" });
    const digest = String(stdout).trim();
    if (!/^sha256:[a-f0-9]{64}$/.test(digest)) throw new Error(`Could not verify the pushed artifact digest for ${item.name}`);
    resolved.push({ ...item, image: `${item.image}@${digest}` });
  }
  return resolved;
}
