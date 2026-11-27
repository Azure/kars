// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { lookup } from "node:dns/promises";
import { BlockList, isIP } from "node:net";
import { loadConfig, promptAndSaveCredentials, type KarsConfig } from "../../config.js";

type Values = Record<string, any>;
export interface KindBetaProviderConnection {
  kind: "openai" | "anthropic" | "github-copilot" | "openai-compatible";
  endpoint: string;
  model: string;
  apiKey?: string;
}
interface Dependencies {
  load: () => KarsConfig | null;
  prompt: typeof promptAndSaveCredentials;
  interactive: boolean;
  resolveAddresses?: (hostname: string) => Promise<string[]>;
}

const hostedEndpoints = {
  openai: "https://api.openai.com/v1",
  // Official chat-completions compatibility, not native advanced Messages support.
  anthropic: "https://api.anthropic.com/v1",
  "github-copilot": "https://api.githubcopilot.com",
};
const nonPublic = new BlockList();
for (const [address, prefix] of [
  ["0.0.0.0", 8], ["10.0.0.0", 8], ["100.64.0.0", 10], ["127.0.0.0", 8],
  ["169.254.0.0", 16], ["172.16.0.0", 12], ["192.0.0.0", 24], ["192.0.2.0", 24],
  ["192.168.0.0", 16], ["198.18.0.0", 15], ["198.51.100.0", 24], ["203.0.113.0", 24],
  ["224.0.0.0", 4], ["240.0.0.0", 4],
] as const) nonPublic.addSubnet(address, prefix, "ipv4");
const globalIPv6 = new BlockList();
globalIPv6.addSubnet("2000::", 3, "ipv6");
nonPublic.addSubnet("2001::", 23, "ipv6");
nonPublic.addSubnet("2001:db8::", 32, "ipv6");
nonPublic.addSubnet("2002::", 16, "ipv6");

export async function publicEndpoint(value: string, dependencies: Pick<Dependencies, "resolveAddresses"> = {}): Promise<string> {
  const endpoint = endpointURL(value), url = new URL(endpoint);
  const host = url.hostname.replace(/^\[|\]$/g, "").replace(/\.$/, "");
  if (!host.includes(".") && !isIP(host) || /(?:^|\.)(?:localhost|local|internal|svc|cluster\.local|home\.arpa)$/.test(host)) {
    throw new Error("Kind beta named providers require a public remote HTTPS endpoint, not a local service");
  }
  let addresses: string[];
  try {
    addresses = isIP(host) ? [host] : await (dependencies.resolveAddresses
      ?? (async hostname => (await lookup(hostname, { all: true })).map(entry => entry.address)))(host);
  } catch { throw new Error("Kind beta provider endpoint must resolve to public remote addresses"); }
  if (!addresses.length || addresses.some(address => {
    const family = isIP(address);
    return family === 4 ? nonPublic.check(address, "ipv4")
      : family !== 6 || !globalIPv6.check(address, "ipv6") || nonPublic.check(address, "ipv6");
  })) throw new Error("Kind beta provider endpoint must resolve only to public remote addresses");
  return endpoint;
}

async function prepareNamedProvider(core: Values, resources: Values[], connection: KindBetaProviderConnection,
  dependencies: Dependencies): Promise<Values> {
  const { kind, model, apiKey } = connection;
  if (!["openai", "anthropic", "github-copilot", "openai-compatible"].includes(kind)) throw new Error("Unsupported explicit Kind beta provider");
  // eslint-disable-next-line no-control-regex -- Model identifiers must not contain ASCII controls.
  if (typeof model !== "string" || !/^[^\s\x00-\x1f\x7f]{1,256}$/.test(model)) throw new Error("Kind beta default model must be a nonempty model identifier without whitespace");
  // eslint-disable-next-line no-control-regex -- Credentials must not contain ASCII controls.
  if (typeof apiKey !== "string" || !apiKey.trim() || /[\x00-\x1f\x7f]/.test(apiKey)) {
    throw new Error("The selected hosted provider requires its own nonempty credential");
  }
  const endpoint = endpointURL(connection.endpoint);
  if (kind !== "openai-compatible" && endpoint !== hostedEndpoints[kind]) throw new Error("Selected provider credentials require the matching official provider endpoint");
  if (kind === "openai-compatible" && new URL(endpoint).hostname === "api.githubcopilot.com") throw new Error("Use github-copilot with its authorized OAuth token for the Copilot endpoint");
  const defaults = core.controller?.inferenceDefaults ?? {};
  if (defaults.provider && defaults.provider !== kind || defaults.model && defaults.model !== model
    || core.sre?.provider && core.sre.provider !== kind || core.sre?.model && core.sre.model !== model) {
    throw new Error("Selected provider/model differs from reviewed Core defaults or SRE overrides");
  }
  const azure = core.inferenceRouter?.azure?.openai;
  if (core.foundry?.endpoint || azure?.endpoint || azure?.credentials?.apiKey || azure?.credentials?.existingSecret) {
    throw new Error("Explicit named provider must not replace a reviewed Azure endpoint or credential binding");
  }
  const prefix = `KARS_PROVIDER_${kind.toUpperCase().replaceAll("-", "_")}_`;
  const data: Record<string, string> = { [`${prefix}ENDPOINT`]: endpoint };
  data[`${prefix}${kind === "github-copilot" ? "TOKEN" : "API_KEY"}`] = apiKey;
  if (kind === "github-copilot") data.COPILOT_GITHUB_TOKEN = apiKey;
  if ((core.controller?.extraEnv ?? []).some((entry: Values) => entry.name?.startsWith(prefix)
    || ["COPILOT_GITHUB_TOKEN", "KARS_TASK_DEFAULT_MODEL", "AZURE_OPENAI_DEPLOYMENT", "KARS_TASK_DEFAULT_PROVIDER"].includes(entry.name))) {
    throw new Error("Controller environment must not shadow the selected provider or Core defaults");
  }
  const name = "kars-inference-providers";
  const matches = resources.filter(resource => resource.kind === "Secret" && resource.metadata?.name === name);
  if (matches.length > 1) throw new Error("Duplicate bootstrap provider Secret");
  const existing = matches[0];
  if (existing) {
    const metadata = existing.metadata;
    if (existing.apiVersion !== "v1" || existing.type !== "Opaque" || metadata.namespace !== "kars-system"
      || metadata.uid || metadata.resourceVersion || metadata.ownerReferences || metadata.finalizers || metadata.deletionTimestamp
      || metadata.annotations?.["kars.azure.com/credential-store-grant-uid"]) {
      throw new Error("Bootstrap provider Secret requires a new Opaque kars-system source identity, not a foreign or enrolled store");
    }
    const stored: Record<string, string> = {};
    for (const field of ["data", "stringData"]) {
      for (const [key, value] of Object.entries(existing[field] ?? {})) {
        if (typeof value !== "string" || Object.hasOwn(stored, key)
          || key !== "COPILOT_GITHUB_TOKEN" && !/^KARS_PROVIDER_[A-Z0-9_]+_(ENDPOINT|API_KEY|TOKEN|MODELS)$/.test(key)) {
          throw new Error("Bootstrap provider Secret contains duplicate or non-provider keys");
        }
        stored[key] = field === "data" ? Buffer.from(value, "base64").toString("utf8") : value;
      }
    }
    const defaultModel = metadata.annotations?.["kars.azure.com/default-model"];
    if (defaultModel !== undefined) {
      let selected: Values;
      try { selected = JSON.parse(defaultModel); } catch { throw new Error("Existing provider default selection is invalid"); }
      if (!selected || selected.provider !== kind || selected.deployment !== model) throw new Error("Existing provider default selection differs from reviewed Core defaults");
    }
    if (Object.entries(data).some(([key, value]) => stored[key] !== value)
      || [`${prefix}API_KEY`, `${prefix}TOKEN`].some(key => Object.hasOwn(stored, key) && !Object.hasOwn(data, key))) {
      throw new Error("Existing provider Secret must match the reviewed endpoint and credential binding exactly; existing data was preserved");
    }
  }
  await publicEndpoint(endpoint, dependencies);
  // Keep credentials private until workspace enrollment completes. Core seals
  // only the public route; the installer fills the enrolled router-only store.
  if (!existing) resources.push({ apiVersion: "v1", kind: "Secret", metadata: { name, namespace: "kars-system" }, type: "Opaque", stringData: data });
  return { controller: { inferenceDefaults: { provider: kind, model } }, kindBeta: { providerEndpoint: endpoint } };
}

function endpointURL(value: string): string {
  const url = new URL(value);
  if (url.protocol !== "https:" || url.username || url.password || url.search || url.hash
    || (url.port && url.port !== "443")) throw new Error("Kind beta inference requires an HTTPS endpoint on port 443 without embedded credentials");
  return url.href.replace(/\/+$/, "");
}

/** Fill only absent Core bootstrap settings; Bridge reads these from Core live. */
export async function prepareKindBetaInference(core: Values, resources: Values[], dependencies: Dependencies = {
  load: loadConfig, prompt: promptAndSaveCredentials, interactive: Boolean(process.stdin.isTTY && process.stdout.isTTY),
}, configureInteractively = false, connection?: KindBetaProviderConnection): Promise<Values> {
  if (connection) return prepareNamedProvider(core, resources, connection, dependencies);
  const defaults = core.controller?.inferenceDefaults ?? {};
  const provider = defaults.provider || core.sre?.provider || (core.foundry?.endpoint ? "foundry" : "azure-openai");
  if (!["foundry", "azure-openai"].includes(provider)) {
    throw new Error("Kind beta bootstrap supports foundry/azure-openai only; other providers require separate qualification");
  }
  const explicitEndpoint = provider === "foundry" ? core.foundry?.endpoint : core.inferenceRouter?.azure?.openai?.endpoint;
  if (explicitEndpoint) endpointURL(explicitEndpoint);
  const explicitModel = defaults.model || core.sre?.model;
  if (explicitModel !== undefined && (typeof explicitModel !== "string" || !explicitModel.trim())) {
    throw new Error("Core default inference model must be a nonempty deployment name");
  }
  const credentials = core.inferenceRouter?.azure?.openai?.credentials ?? {};
  if (credentials.apiKey) throw new Error("Kind beta requires an inference credential Secret reference, not an inline key");
  if (credentials.existingSecret) {
    const secret = resources.find(resource => resource.kind === "Secret" && resource.metadata?.name === credentials.existingSecret);
    const key = credentials.key || "api-key";
    if (!(secret?.data?.[key] || secret?.stringData?.[key])) throw new Error(`Bootstrap Secret ${credentials.existingSecret} must contain ${key}`);
    if (!explicitEndpoint || !explicitModel) {
      throw new Error("Prepared inference credentials require the matching Core endpoint and controller.inferenceDefaults.model (or explicit sre.model); saved laptop settings will not replace this route");
    }
    return { controller: { inferenceDefaults: { provider, model: explicitModel } } };
  }

  const secretName = "kars-kind-inference";
  if (resources.some(resource => resource.kind === "Secret" && resource.metadata?.name === secretName)) {
    throw new Error("kars-kind-inference already exists in bootstrap inputs; reference it explicitly rather than replacing it");
  }
  const saved = dependencies.load();
  const compatible = saved && (saved.provider === "foundry" || saved.provider === undefined)
    && saved.endpoint && saved.apiKey && saved.model
    && (!explicitEndpoint || endpointURL(saved.endpoint) === endpointURL(explicitEndpoint));
  let selected: KarsConfig;
  if (compatible && !configureInteractively) selected = saved;
  else {
    if (!dependencies.interactive) {
      throw new Error("Missing Kind beta inference settings. Run interactively to choose the endpoint, API-key authentication and default model, or supply reviewed Core inference values and their credential Secret");
    }
    selected = await dependencies.prompt({ provider: "foundry", heading: "Kind beta Core inference setup",
      foundry: { endpoint: explicitEndpoint, model: explicitModel } });
  }
  if (selected.provider && selected.provider !== "foundry") throw new Error("Kind beta requires the qualified Foundry/Azure OpenAI bootstrap route");
  endpointURL(selected.endpoint);
  if (!selected.apiKey || !selected.model?.trim()) throw new Error("Inference authentication and default model are required");
  if (explicitEndpoint && endpointURL(selected.endpoint) !== endpointURL(explicitEndpoint)) {
    throw new Error("Selected credentials do not match the reviewed Core endpoint; no bootstrap credentials were created");
  }
  const endpoint = explicitEndpoint || selected.endpoint;
  const model = explicitModel || selected.model;
  const key = credentials.key || "api-key";
  const overlay: Values = {
    controller: { inferenceDefaults: { provider, model } },
    inferenceRouter: { azure: { openai: {
      ...(provider === "azure-openai" ? { endpoint } : {}),
      credentials: { existingSecret: secretName, key },
    } } },
  };
  if (provider === "foundry") overlay.foundry = { endpoint };
  if (core.foundry?.projectEndpoint === undefined && selected.foundryProjectEndpoint) {
    overlay.foundry = { ...overlay.foundry, projectEndpoint: selected.foundryProjectEndpoint };
  }
  resources.push({ apiVersion: "v1", kind: "Secret", metadata: { name: secretName, namespace: "kars-system" },
    type: "Opaque", stringData: { [key]: selected.apiKey } });
  return overlay;
}
