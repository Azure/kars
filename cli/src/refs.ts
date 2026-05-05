// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// S13 phase2-config-authority-refs:
// `ClawSandbox.spec.inferenceRef` → sibling `InferencePolicy` CR (required).
// `ClawSandbox.spec.governance.toolPolicyRef` → sibling `ToolPolicy` CR
// (required when governance.enabled). Both are SAME-NAMESPACE refs.

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
      requirePromptShields: opts.promptShields !== false,
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
    apiVersion: "azureclaw.azure.com/v1alpha1",
    kind: "InferencePolicy",
    metadata: {
      name: inferenceRefName(opts.sandboxName),
      namespace: opts.namespace,
      labels: { "azureclaw.azure.com/sandbox": opts.sandboxName },
    },
    spec,
  };
}

export interface ToolPolicyOpts {
  sandboxName: string;
  namespace: string;
  profile?: string;
}

export function buildToolPolicy(opts: ToolPolicyOpts): Record<string, unknown> {
  return {
    apiVersion: "azureclaw.azure.com/v1alpha1",
    kind: "ToolPolicy",
    metadata: {
      name: toolPolicyRefName(opts.sandboxName),
      namespace: opts.namespace,
      labels: { "azureclaw.azure.com/sandbox": opts.sandboxName },
      annotations: opts.profile
        ? { "azureclaw.azure.com/profile": opts.profile }
        : undefined,
    },
    spec: {
      // ToolPolicy.appliesTo has no sandboxName field — scope via sandbox label.
      appliesTo: {
        sandboxMatchLabels: { "azureclaw.azure.com/sandbox": opts.sandboxName },
      },
    },
  };
}
