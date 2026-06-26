// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// S13 phase2-config-authority-refs:
// `KarsSandbox.spec.inferenceRef` → sibling `InferencePolicy` CR (required).
// `KarsSandbox.spec.governance.toolPolicyRef` → sibling `ToolPolicy` CR
// (required when governance.enabled). Both are SAME-NAMESPACE refs.

import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const DNS_LABEL_MAX = 63;

// Truncate a base name + suffix so the result is a valid DNS-1123 subdomain
// (max 63 chars per label). The suffix already contains a leading hyphen.
export function kebabRefName(base: string, suffix: string): string {
  const b = base.toLowerCase().replace(/[^a-z0-9-]/g, "-").replace(/^-+|-+$/g, "");
  const max = DNS_LABEL_MAX - suffix.length;
  const trimmed = b.length > max ? b.slice(0, max).replace(/-+$/g, "") : b;
  return `${trimmed}${suffix}`;
}

export const inferenceRefName = (sandboxName: string) =>
  kebabRefName(sandboxName, "-inference");

export const toolPolicyRefName = (sandboxName: string) =>
  kebabRefName(sandboxName, "-toolpolicy");

export const memoryRefName = (sandboxName: string) =>
  kebabRefName(sandboxName, "-memory");

/** Foundry Memory Store name for a sandbox — matches the runtime convention
 *  `memory-<sandbox>` in runtimes/openclaw memory-binding.ts. DNS-label safe. */
export const memoryStoreName = (sandboxName: string) =>
  kebabRefName(sandboxName, "").replace(/^/, "memory-").slice(0, 63).replace(/-+$/g, "");

export interface KarsMemoryOpts {
  sandboxName: string;
  namespace: string;
  retentionDays?: number;
}

/**
 * Build a KarsMemory CR so a `kars up` sandbox gets the same controller-managed
 * Foundry Memory Store binding that `kars dev` already creates. Without it the
 * runtime falls back to lazy store creation with no declarative binding.
 */
export function buildKarsMemory(opts: KarsMemoryOpts): Record<string, unknown> {
  const store = memoryStoreName(opts.sandboxName);
  return {
    apiVersion: "kars.azure.com/v1alpha1",
    kind: "KarsMemory",
    metadata: {
      name: memoryRefName(opts.sandboxName),
      namespace: opts.namespace,
      labels: { "kars.azure.com/sandbox": opts.sandboxName },
    },
    spec: {
      sandboxRef: { name: opts.sandboxName },
      storeName: store,
      scope: `agent_${opts.sandboxName}`,
      retentionDays: opts.retentionDays ?? 30,
      deleteOnSandboxDelete: true,
      displayName: `Default memory for ${opts.sandboxName}`,
    },
  };
}

export interface InferencePolicyOpts {
  sandboxName: string;
  namespace: string;
  model: string;
  provider?: string;
  contentSafety?: boolean;
  promptShields?: boolean;
  tokenBudgetDaily?: number;
  tokenBudgetPerRequest?: number;
}

export function buildInferencePolicy(opts: InferencePolicyOpts): Record<string, unknown> {
  // Endpoint URLs (AOAI / Foundry) are NOT carried on this CR — they are
  // sourced cluster-wide from helm values into router pod env vars.
  // See docs/adr/0002-inference-endpoint-sourcing.md before adding any
  // `endpoint`/`endpointOverride` field here.
  const spec: Record<string, unknown> = {
    appliesTo: { sandboxName: opts.sandboxName },
    modelPreference: {
      primary: {
        provider: opts.provider ?? "azure-openai",
        deployment: opts.model,
      },
    },
    contentSafety: {
      // Default OFF: bare Foundry / Azure OpenAI deployments without an
      // attached Content Filter do NOT emit `prompt_filter_results`, so a
      // fail-closed requirement blocks every response. Opt in explicitly
      // (CLI `--require-prompt-shields`) only when the deployment has a
      // Content Filter that surfaces those annotations.
      requirePromptShields: opts.promptShields === true,
    },
  };
  const daily = opts.tokenBudgetDaily ?? 0;
  const perReq = opts.tokenBudgetPerRequest ?? 0;
  if (daily > 0 || perReq > 0) {
    const tb: Record<string, unknown> = {};
    if (daily > 0) tb.dailyTokens = daily;
    if (perReq > 0) tb.perRequestTokens = perReq;
    spec.tokenBudget = tb;
  }
  return {
    apiVersion: "kars.azure.com/v1alpha1",
    kind: "InferencePolicy",
    metadata: {
      name: inferenceRefName(opts.sandboxName),
      namespace: opts.namespace,
      labels: { "kars.azure.com/sandbox": opts.sandboxName },
    },
    spec,
  };
}

export interface ToolPolicyOpts {
  sandboxName: string;
  namespace: string;
  profile?: string;
}

/**
 * Resolve the AGT profile YAML bytes that get inlined into
 * `ToolPolicy.spec.agtProfile.inline`. Pre-Slice-1e the sandbox image
 * shipped these YAMLs at `/opt/kars-plugin/policies/` and the
 * controller selected one by name via `AGT_POLICY_PROFILE`. Post-Slice-1e
 * (phase 2) the bundled sandbox-side path is gone — the CLI is the
 * sole producer of the inline policy bytes, and the controller
 * hard-fails any ToolPolicy without `spec.agtProfile.inline`.
 *
 * Profile assets live in `cli/profiles/agt/kars-<profile>.yaml`
 * and are copied to `dist/profiles/agt/` by the build script. We
 * resolve relative to `import.meta.url` so the lookup works whether
 * the CLI runs from source (tsx/vitest) or from the installed bundle.
 *
 * Unknown profile names fall back to `default` with a console warning
 * so a typo doesn't silently produce an empty inline policy.
 */
export function loadAgtProfile(profile: string): string {
  const requested = (profile || "default").trim() || "default";
  const candidate = (name: string) => {
    const here = fileURLToPath(import.meta.url);
    // dist build  → .../dist/refs.js → look in .../dist/profiles/agt
    // tsx / tests → .../src/refs.ts  → look in .../profiles/agt
    const dirs = [
      path.resolve(path.dirname(here), "profiles", "agt"),
      path.resolve(path.dirname(here), "..", "profiles", "agt"),
    ];
    for (const d of dirs) {
      const p = path.join(d, `kars-${name}.yaml`);
      try {
        return readFileSync(p, "utf8");
      } catch {
        // try next
      }
    }
    return undefined;
  };

  const direct = candidate(requested);
  if (direct !== undefined) return direct;

  if (requested !== "default") {
    console.warn(
      `[kars] AGT profile '${requested}' not found; falling back to 'default'`,
    );
    const fallback = candidate("default");
    if (fallback !== undefined) return fallback;
  }

  throw new Error(
    `AGT profile asset 'kars-${requested}.yaml' not found in cli/profiles/agt/. ` +
      `The CLI build is missing its bundled profiles — re-run \`npm run build\` ` +
      `or reinstall the CLI.`,
  );
}

export function buildToolPolicy(opts: ToolPolicyOpts): Record<string, unknown> {
  const profile = (opts.profile || "default").trim() || "default";
  const inline = loadAgtProfile(profile);
  return {
    apiVersion: "kars.azure.com/v1alpha1",
    kind: "ToolPolicy",
    metadata: {
      name: toolPolicyRefName(opts.sandboxName),
      namespace: opts.namespace,
      labels: { "kars.azure.com/sandbox": opts.sandboxName },
      annotations: { "kars.azure.com/profile": profile },
    },
    spec: {
      // ToolPolicy.appliesTo has no sandboxName field — scope via sandbox label.
      appliesTo: {
        sandboxMatchLabels: { "kars.azure.com/sandbox": opts.sandboxName },
      },
      agtProfile: { inline },
    },
  };
}
