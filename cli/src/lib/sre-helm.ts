// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { Execute } from "./sre-authority.js";

export function sreInferenceFlags(options: { provider?: string; model?: string }, previousValues?: unknown): string[] {
  const flags: string[] = [];
  for (const key of ["provider", "model"] as const) {
    const value = options[key];
    if (value === undefined) continue;
    if (!value.trim() || value !== value.trim()) {
      throw new Error(`SRE --${key} must be nonblank with no surrounding whitespace.`);
    }
    if (key === "provider" && !/^[a-zA-Z0-9][a-zA-Z0-9_-]*$/.test(value)) {
      throw new Error("SRE --provider must be a configured provider tag (letters, digits, hyphens or underscores).");
    }
    // JSON scalars keep model names containing commas/braces from becoming
    // additional Helm assignments or lists, without replacing other SRE values.
    flags.push("--set-json", `sre.${key}=${JSON.stringify(value)}`);
  }
  if (flags.length && previousValues !== undefined) {
    if (!previousValues || typeof previousValues !== "object" || Array.isArray(previousValues)) {
      throw new Error("SRE model selection requires valid existing Helm values.");
    }
    const sre = "sre" in previousValues ? previousValues.sre : {};
    if (!sre || typeof sre !== "object" || Array.isArray(sre)) {
      throw new Error("SRE model selection requires valid existing SRE values.");
    }
    const baseline = "modelRouteBaseline" in sre ? sre.modelRouteBaseline : {
      provider: "provider" in sre ? sre.provider : "",
      model: "model" in sre ? sre.model : "",
    };
    if (!baseline || typeof baseline !== "object" || Array.isArray(baseline)
        || Object.keys(baseline).length !== 2
        || !("provider" in baseline) || typeof baseline.provider !== "string"
        || !("model" in baseline) || typeof baseline.model !== "string") {
      throw new Error("SRE model selection requires the original provider/model string baseline.");
    }
    flags.push("--set-json", `sre.modelRouteBaseline=${JSON.stringify(baseline)}`);
  }
  return flags;
}

export async function sreHelmMajor(execute: Execute): Promise<3 | 4> {
  const { stdout } = await execute("helm", ["version", "--template", "{{.Version}}"], { stdio: "pipe" });
  const match = /^v([34])\.\d+\.\d+(?:[-+][0-9A-Za-z.+-]+)?$/.exec(stdout.trim());
  if (!match) throw new Error("Unsupported Helm version; SRE staging requires Helm 3 or 4.");
  return Number(match[1]) as 3 | 4;
}

export async function sreHelmStageWait(execute: Execute): Promise<string> {
  // The Helm 4 watcher waits for custom-resource Ready, which requires the
  // explicit enrollment AFTER staging. Its legacy waiter checks built-ins/APIs.
  return await sreHelmMajor(execute) === 4 ? "--wait=legacy" : "--wait";
}

function removedAllFlag(error: unknown): boolean {
  return error instanceof Error
    && "exitCode" in error && error.exitCode === 1
    && "stderr" in error && typeof error.stderr === "string"
    && error.stderr.trim() === "Error: unknown flag: --all";
}

export async function listSreHelmReleases(execute: Execute, namespace: string): Promise<string> {
  try {
    return (await execute("helm", ["list", "-n", namespace, "--all", "-o", "json"], { stdio: "pipe" })).stdout;
  } catch (error) {
    if (!removedAllFlag(error)) throw error;
    if (await sreHelmMajor(execute) !== 4) {
      throw new Error("Unsupported Helm release inventory: only Helm 4 can omit the --all flag.", { cause: error });
    }
    // Helm 3 needs --all; Helm 4 removed it and lists every status by default.
    console.warn("Helm 4 lists all release statuses by default; retrying without the removed --all flag.");
    return (await execute("helm", ["list", "-n", namespace, "-o", "json"], { stdio: "pipe" })).stdout;
  }
}
