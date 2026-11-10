// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { loadConfig, promptAndSaveCredentials, type KarsConfig } from "../../config.js";

type Values = Record<string, any>;
interface Dependencies {
  load: () => KarsConfig | null;
  prompt: typeof promptAndSaveCredentials;
  interactive: boolean;
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
}, configureInteractively = false): Promise<Values> {
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
