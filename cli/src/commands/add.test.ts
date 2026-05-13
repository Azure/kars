// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";

/**
 * Tests for the `add` command's sandbox manifest generation logic.
 *
 * The action handler in add.ts builds a ClawSandbox object inline, so we
 * extract and test the manifest-building logic by replicating the pure
 * data-transformation portion (no kubectl/execa calls).
 */

// --- Helpers that mirror the data-building logic in add.ts ---

interface AddOptions {
  model: string;
  isolation: string;
  tokenBudgetDaily: string;
  tokenBudgetPerRequest: string;
  image?: string;
  agentInstructions?: string;
  agentTools?: string;
  governance: boolean;
  policyProfile: string;
  trustThreshold: string;
  channels?: string;
  telegramToken?: string;
  slackToken?: string;
  discordToken?: string;
  braveApiKey?: string;
  tavilyApiKey?: string;
  exaApiKey?: string;
  firecrawlApiKey?: string;
  perplexityApiKey?: string;
  openaiApiKey?: string;
  learnEgress: boolean;
  skills?: string;
}

function defaultOptions(overrides: Partial<AddOptions> = {}): AddOptions {
  return {
    model: "gpt-4.1",
    isolation: "enhanced",
    tokenBudgetDaily: "0",
    tokenBudgetPerRequest: "0",
    governance: true,
    policyProfile: "default",
    trustThreshold: "500",
    learnEgress: false,
    ...overrides,
  };
}

/** Build the ClawSandbox manifest object (mirrors add.ts action logic). */
function buildSandboxManifest(name: string, options: AddOptions) {
  const sandbox: Record<string, unknown> = {
    apiVersion: "azureclaw.azure.com/v1alpha1",
    kind: "ClawSandbox",
    metadata: { name, namespace: "azureclaw-system" },
    spec: {
      runtime: {
        kind: "OpenClaw",
        openclaw: {
          version: "2026.3.13",
          ...(options.image ? { image: options.image } : {}),
          config: { agent: { model: `azure/${options.model}` } },
        },
      },
      sandbox: {
        isolation: options.isolation,
        seccompProfile: options.isolation === "standard" ? "RuntimeDefault" : "azureclaw-strict",
        readOnlyRootFilesystem: true,
        runAsNonRoot: true,
        allowPrivilegeEscalation: false,
        writablePaths: ["/sandbox", "/tmp"],
      },
      inferenceRef: {
        name: `${name}-inference`,
      },
      networkPolicy: {
        defaultDeny: true,
        approvalRequired: true,
        allowedEndpoints: [
          { host: "github.com", port: 443 },
          { host: "api.github.com", port: 443 },
        ],
      },
      resources: {
        requests: { cpu: "500m", memory: "1Gi" },
        limits: { cpu: "2", memory: "4Gi" },
      },
    },
  };

  if (options.agentInstructions || options.agentTools) {
    const agentSpec: Record<string, unknown> = {};
    if (options.agentInstructions) agentSpec.instructions = options.agentInstructions;
    if (options.agentTools)
      agentSpec.tools = options.agentTools.split(",").map((t: string) => t.trim());
    (sandbox.spec as Record<string, unknown>).agent = agentSpec;
  }

  if (options.governance) {
    (sandbox.spec as Record<string, unknown>).governance = {
      enabled: true,
      toolPolicyRef: { name: `${name}-toolpolicy` },
      trustThreshold: parseInt(options.trustThreshold) || 500,
    };
  }

  if (options.learnEgress) {
    const np = (sandbox.spec as Record<string, unknown>).networkPolicy as Record<string, unknown>;
    np.egressMode = "Learn";
  }

  return sandbox;
}

/** Build channel/plugin env secret map (mirrors add.ts logic). */
function buildSecrets(options: AddOptions) {
  const channelTokenFlags: Record<string, string> = {
    telegram: "telegramToken",
    slack: "slackToken",
    discord: "discordToken",
  };
  const channelEnvVars: Record<string, string> = {
    telegram: "TELEGRAM_BOT_TOKEN",
    slack: "SLACK_BOT_TOKEN",
    discord: "DISCORD_BOT_TOKEN",
    whatsapp: "WHATSAPP_ENABLED",
  };
  const knownChannels = new Set(["telegram", "slack", "discord", "whatsapp"]);

  const channelEnvSecrets: Record<string, string> = {};
  if (options.channels) {
    const channels = options.channels.split(",").map((c: string) => c.trim().toLowerCase());
    for (const channel of channels) {
      if (!knownChannels.has(channel)) continue;
      const tokenFlag = channelTokenFlags[channel];
      if (tokenFlag && (options as any)[tokenFlag]) {
        channelEnvSecrets[channelEnvVars[channel]] = (options as any)[tokenFlag];
      } else if (channel === "whatsapp") {
        channelEnvSecrets[channelEnvVars[channel]] = "true";
      }
    }
  }

  const pluginKeyFlags: Record<string, { flag: string; env: string }> = {
    brave: { flag: "braveApiKey", env: "BRAVE_API_KEY" },
    tavily: { flag: "tavilyApiKey", env: "TAVILY_API_KEY" },
    exa: { flag: "exaApiKey", env: "EXA_API_KEY" },
    firecrawl: { flag: "firecrawlApiKey", env: "FIRECRAWL_API_KEY" },
    perplexity: { flag: "perplexityApiKey", env: "PERPLEXITY_API_KEY" },
    openai: { flag: "openaiApiKey", env: "OPENAI_API_KEY" },
  };
  const pluginSecrets: Record<string, string> = {};
  for (const [, { flag, env }] of Object.entries(pluginKeyFlags)) {
    if ((options as any)[flag]) {
      pluginSecrets[env] = (options as any)[flag];
    }
  }

  return { ...channelEnvSecrets, ...pluginSecrets };
}

// --- Tests ---

describe("ClawSandbox manifest generation", () => {
  it("generates correct apiVersion and kind", () => {
    const manifest = buildSandboxManifest("agent1", defaultOptions());
    expect(manifest.apiVersion).toBe("azureclaw.azure.com/v1alpha1");
    expect(manifest.kind).toBe("ClawSandbox");
  });

  it("sets metadata name and namespace", () => {
    const manifest = buildSandboxManifest("my-agent", defaultOptions());
    expect(manifest.metadata).toEqual({
      name: "my-agent",
      namespace: "azureclaw-system",
    });
  });

  it("uses default model gpt-4.1 with azure/ prefix", () => {
    const manifest = buildSandboxManifest("a", defaultOptions());
    const spec = manifest.spec as any;
    expect(spec.runtime.kind).toBe("OpenClaw");
    expect(spec.runtime.openclaw.config.agent.model).toBe("azure/gpt-4.1");
    expect(spec.inferenceRef.name).toBe("a-inference");
  });

  it("uses custom model when specified", () => {
    const manifest = buildSandboxManifest("a", defaultOptions({ model: "o4-mini" }));
    const spec = manifest.spec as any;
    expect(spec.runtime.openclaw.config.agent.model).toBe("azure/o4-mini");
    // S13: model is carried on the sibling InferencePolicy CR, not the
    // sandbox spec. The runtime block still seeds OpenClaw config.
    expect(spec.inferenceRef.name).toBe("a-inference");
  });

  it("sets standard isolation with RuntimeDefault seccomp", () => {
    const manifest = buildSandboxManifest("a", defaultOptions({ isolation: "standard" }));
    const spec = manifest.spec as any;
    expect(spec.sandbox.isolation).toBe("standard");
    expect(spec.sandbox.seccompProfile).toBe("RuntimeDefault");
  });

  it("sets enhanced isolation with azureclaw-strict seccomp", () => {
    const manifest = buildSandboxManifest("a", defaultOptions({ isolation: "enhanced" }));
    const spec = manifest.spec as any;
    expect(spec.sandbox.isolation).toBe("enhanced");
    expect(spec.sandbox.seccompProfile).toBe("azureclaw-strict");
  });

  it("sets confidential isolation with azureclaw-strict seccomp", () => {
    const manifest = buildSandboxManifest("a", defaultOptions({ isolation: "confidential" }));
    const spec = manifest.spec as any;
    expect(spec.sandbox.isolation).toBe("confidential");
    expect(spec.sandbox.seccompProfile).toBe("azureclaw-strict");
  });

  it("references the InferencePolicy CR by name (token budget lives on the policy)", () => {
    const manifest = buildSandboxManifest(
      "a",
      defaultOptions({ tokenBudgetDaily: "100000", tokenBudgetPerRequest: "4096" }),
    );
    const spec = manifest.spec as any;
    // S13 phase2-config-authority-refs: budgets are carried on the
    // sibling InferencePolicy CR, not inline on the sandbox spec.
    expect(spec.inferenceRef.name).toBe("a-inference");
    expect(spec.inference).toBeUndefined();
  });

  it("does not emit an inline inference block (S13 refs-only)", () => {
    const manifest = buildSandboxManifest("a", defaultOptions());
    const spec = manifest.spec as any;
    expect(spec.inference).toBeUndefined();
    expect(spec.inferenceRef).toEqual({ name: "a-inference" });
  });

  it("includes custom image when specified", () => {
    const manifest = buildSandboxManifest(
      "a",
      defaultOptions({ image: "myregistry.azurecr.io/custom:v1" }),
    );
    const spec = manifest.spec as any;
    expect(spec.runtime.openclaw.image).toBe("myregistry.azurecr.io/custom:v1");
  });

  it("omits image field when not specified", () => {
    const manifest = buildSandboxManifest("a", defaultOptions());
    const spec = manifest.spec as any;
    expect(spec.runtime.openclaw.image).toBeUndefined();
  });

  it("includes default network policy with github endpoints", () => {
    const manifest = buildSandboxManifest("a", defaultOptions());
    const spec = manifest.spec as any;
    expect(spec.networkPolicy.defaultDeny).toBe(true);
    expect(spec.networkPolicy.allowedEndpoints).toEqual([
      { host: "github.com", port: 443 },
      { host: "api.github.com", port: 443 },
    ]);
  });

  it("sets egressMode=Learn when --learn-egress flag is set (Slice 5b)", () => {
    const manifest = buildSandboxManifest("a", defaultOptions({ learnEgress: true }));
    const spec = manifest.spec as any;
    expect(spec.networkPolicy.egressMode).toBe("Learn");
  });

  it("adds governance config when enabled (S13: toolPolicyRef)", () => {
    const manifest = buildSandboxManifest(
      "a",
      defaultOptions({ governance: true, trustThreshold: "750", policyProfile: "strict" }),
    );
    const spec = manifest.spec as any;
    expect(spec.governance).toEqual({
      enabled: true,
      toolPolicyRef: { name: "a-toolpolicy" },
      trustThreshold: 750,
    });
  });

  it("omits governance when disabled", () => {
    const manifest = buildSandboxManifest("a", defaultOptions({ governance: false }));
    const spec = manifest.spec as any;
    expect(spec.governance).toBeUndefined();
  });

  it("adds agent instructions and tools", () => {
    const manifest = buildSandboxManifest(
      "a",
      defaultOptions({
        agentInstructions: "You are a helpful assistant",
        agentTools: "file_search, code_interpreter",
      }),
    );
    const spec = manifest.spec as any;
    expect(spec.agent.instructions).toBe("You are a helpful assistant");
    expect(spec.agent.tools).toEqual(["file_search", "code_interpreter"]);
  });

  it("enforces security defaults on sandbox spec", () => {
    const manifest = buildSandboxManifest("a", defaultOptions());
    const spec = manifest.spec as any;
    expect(spec.sandbox.readOnlyRootFilesystem).toBe(true);
    expect(spec.sandbox.runAsNonRoot).toBe(true);
    expect(spec.sandbox.allowPrivilegeEscalation).toBe(false);
    expect(spec.sandbox.writablePaths).toEqual(["/sandbox", "/tmp"]);
  });
});

describe("channel and plugin secret generation", () => {
  it("maps telegram token to TELEGRAM_BOT_TOKEN env var", () => {
    const secrets = buildSecrets(
      defaultOptions({ channels: "telegram", telegramToken: "123:ABC" }),
    );
    expect(secrets.TELEGRAM_BOT_TOKEN).toBe("123:ABC");
  });

  it("maps slack token to SLACK_BOT_TOKEN env var", () => {
    const secrets = buildSecrets(
      defaultOptions({ channels: "slack", slackToken: "xoxb-test" }),
    );
    expect(secrets.SLACK_BOT_TOKEN).toBe("xoxb-test");
  });

  it("maps discord token to DISCORD_BOT_TOKEN env var", () => {
    const secrets = buildSecrets(
      defaultOptions({ channels: "discord", discordToken: "disc-tok" }),
    );
    expect(secrets.DISCORD_BOT_TOKEN).toBe("disc-tok");
  });

  it("sets WHATSAPP_ENABLED for whatsapp channel", () => {
    const secrets = buildSecrets(defaultOptions({ channels: "whatsapp" }));
    expect(secrets.WHATSAPP_ENABLED).toBe("true");
  });

  it("handles multiple channels at once", () => {
    const secrets = buildSecrets(
      defaultOptions({
        channels: "telegram,slack,whatsapp",
        telegramToken: "tg-tok",
        slackToken: "sl-tok",
      }),
    );
    expect(secrets.TELEGRAM_BOT_TOKEN).toBe("tg-tok");
    expect(secrets.SLACK_BOT_TOKEN).toBe("sl-tok");
    expect(secrets.WHATSAPP_ENABLED).toBe("true");
  });

  it("skips unknown channels", () => {
    const secrets = buildSecrets(defaultOptions({ channels: "teams,telegram", telegramToken: "t" }));
    expect(secrets.TELEGRAM_BOT_TOKEN).toBe("t");
    expect(Object.keys(secrets)).not.toContain("TEAMS_BOT_TOKEN");
  });

  it("maps brave API key to BRAVE_API_KEY", () => {
    const secrets = buildSecrets(defaultOptions({ braveApiKey: "brave-123" }));
    expect(secrets.BRAVE_API_KEY).toBe("brave-123");
  });

  it("maps multiple plugin keys simultaneously", () => {
    const secrets = buildSecrets(
      defaultOptions({
        tavilyApiKey: "tav-key",
        exaApiKey: "exa-key",
        firecrawlApiKey: "fc-key",
        perplexityApiKey: "pplx-key",
        openaiApiKey: "oai-key",
      }),
    );
    expect(secrets.TAVILY_API_KEY).toBe("tav-key");
    expect(secrets.EXA_API_KEY).toBe("exa-key");
    expect(secrets.FIRECRAWL_API_KEY).toBe("fc-key");
    expect(secrets.PERPLEXITY_API_KEY).toBe("pplx-key");
    expect(secrets.OPENAI_API_KEY).toBe("oai-key");
  });

  it("returns empty object when no channels or plugins set", () => {
    const secrets = buildSecrets(defaultOptions());
    expect(secrets).toEqual({});
  });

  it("merges channel and plugin secrets together", () => {
    const secrets = buildSecrets(
      defaultOptions({
        channels: "telegram",
        telegramToken: "tg",
        braveApiKey: "brave",
      }),
    );
    expect(secrets.TELEGRAM_BOT_TOKEN).toBe("tg");
    expect(secrets.BRAVE_API_KEY).toBe("brave");
  });
});
