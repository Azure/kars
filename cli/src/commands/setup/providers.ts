// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { isIP } from "node:net";
import { COPILOT_API_ENDPOINT, COPILOT_TOKEN_URL } from "../../github-copilot.js";
import { SetupError, type SetupModel, type SetupProvider } from "./types.js";
import { publicEndpoint } from "../dev/kind-beta-inference.js";

const endpoints = { openai: "https://api.openai.com/v1", anthropic: "https://api.anthropic.com/v1", "github-copilot": COPILOT_API_ENDPOINT };
// eslint-disable-next-line no-control-regex -- Reject ASCII controls in externally supplied configuration.
const safeText = (value: unknown, max: number): value is string => typeof value === "string" && value.length <= max && !/[\x00-\x1f\x7f]/.test(value);

export function validateProvider(provider: SetupProvider): void {
  if (!provider || !["foundry", "azure-openai", "openai", "anthropic", "github-copilot", "openai-compatible"].includes(provider.kind)
    || !safeText(provider.endpoint, 2048) || !safeText(provider.model, 256) || !/^[A-Za-z0-9][A-Za-z0-9._:/-]*$/.test(provider.model)
    || (provider.apiKey !== undefined && !safeText(provider.apiKey, 4096))) throw new SetupError("Enter a supported inference provider, endpoint and model");
  let url: URL;
  try { url = new URL(provider.endpoint); } catch { throw new SetupError("Inference endpoint must be a valid HTTPS URL"); }
  if (url.protocol !== "https:" || url.username || url.password || url.search || url.hash || url.port) {
    throw new SetupError("Use HTTPS on port 443 without URL credentials, query parameters or fragments");
  }
  if (provider.kind === "foundry" || provider.kind === "azure-openai") {
    const suffix = provider.kind === "azure-openai" ? ".openai.azure.com" : ".services.ai.azure.com";
    if (!url.hostname.endsWith(suffix) || !/^[a-z0-9-]+$/.test(url.hostname.slice(0, -suffix.length))
      || url.pathname !== "/" || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(provider.model)) {
      throw new SetupError("Use the public Azure resource endpoint and deployment name; private, sovereign and project endpoints need reviewed CLI configuration");
    }
  } else if (provider.kind === "openai-compatible") {
    if (isIP(url.hostname.replace(/^\[|\]$/g, "")) || !url.hostname.includes(".")
      || /(?:^|\.)(?:localhost|local|internal|test|invalid|svc|cluster\.local)$/.test(url.hostname)
      || url.hostname.endsWith(".") || !/^[a-z0-9.-]+$/.test(url.hostname)
      || !/^\/(?:[A-Za-z0-9_-]+\/)*[A-Za-z0-9_-]*$/.test(url.pathname)) {
      throw new SetupError("This installer connects public HTTPS model endpoints. In-cluster KAITO/Ollama needs an existing service and precise egress configuration; localhost is not reachable from a new Kind sandbox.");
    }
  } else if (url.href.replace(/\/+$/, "") !== endpoints[provider.kind]) {
    throw new SetupError("Use this provider's official API endpoint; choose OpenAI-compatible for a custom server");
  }
}

async function responseJSON(response: Response, maxBytes = 65_536): Promise<any> {
  if (!response.ok) {
    await response.body?.cancel();
    throw new SetupError(`Inference check returned HTTP ${response.status}. Check provider access, model, credentials and quota; no cluster was installed.`);
  }
  const reader = response.body?.getReader();
  if (!reader) throw new Error("Missing response");
  let size = 0;
  const chunks: Uint8Array[] = [];
  try {
    while (true) {
      const part = await reader.read();
      if (part.done) break;
      size += part.value.length;
      if (size > maxBytes) throw new Error("Oversized response");
      chunks.push(part.value);
    }
  } finally { await reader.cancel(); }
  return JSON.parse(Buffer.concat(chunks).toString("utf8"));
}

const copilotHeaders = { "User-Agent": "GitHubCopilotChat/0.26.7", "Editor-Version": "vscode/1.99.0", "Editor-Plugin-Version": "copilot-chat/0.26.7", "Copilot-Integration-Id": "vscode-chat" };
async function copilotBearer(oauth: string, request: typeof fetch, signal: AbortSignal): Promise<string> {
  const exchanged = await responseJSON(await request(COPILOT_TOKEN_URL, {
    redirect: "error", signal, headers: { ...copilotHeaders, Authorization: `token ${oauth}`, Accept: "application/json" },
  }));
  // eslint-disable-next-line no-control-regex -- A bearer token must not contain ASCII controls or spaces.
  if (typeof exchanged.token !== "string" || !exchanged.token || /[\x00-\x20\x7f]/.test(exchanged.token) || exchanged.chat_enabled === false) throw new Error("Copilot chat unavailable");
  return exchanged.token;
}

async function copilotModels(key: string, signal: AbortSignal, request: typeof fetch): Promise<Array<SetupModel & { endpoint: "/chat/completions" | "/responses" }>> {
  const result = await responseJSON(await request(`${COPILOT_API_ENDPOINT}/models`, {
    redirect: "error", signal, headers: { ...copilotHeaders, Authorization: `Bearer ${key}`, Accept: "application/json" },
  }), 1024 * 1024);
  if (!Array.isArray(result.data)) throw new Error("Invalid model catalog");
  const models = new Map<string, SetupModel & { endpoint: "/chat/completions" | "/responses" }>();
  // Match Bridge's seat-policy filters and the shared router's supported protocols.
  for (const value of result.data as unknown[]) {
    if (!value || typeof value !== "object") continue;
    const model = value as Record<string, unknown>;
    const capabilities = model.capabilities as { type?: unknown } | undefined;
    const policy = model.policy as { state?: unknown } | undefined;
    const endpoints = model.supported_endpoints;
    const endpoint = endpoints === undefined || Array.isArray(endpoints) && endpoints.includes("/chat/completions") ? "/chat/completions"
      : Array.isArray(endpoints) && endpoints.includes("/responses") ? "/responses" : undefined;
    if (capabilities?.type !== "chat" || model.model_picker_enabled !== true
      || (policy !== undefined && policy?.state !== "enabled")
      || !endpoint
      || !safeText(model.id, 256) || !/^[A-Za-z0-9][A-Za-z0-9._:/-]*$/.test(model.id)) continue;
    models.set(model.id, { id: model.id, name: safeText(model.name, 256) && model.name ? model.name : model.id, endpoint });
  }
  if (!models.size) throw new Error("No supported chat models");
  return [...models.values()].sort((a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id));
}

export async function discoverSetupCopilotModels(oauth: string, signal: AbortSignal, request: typeof fetch = fetch): Promise<SetupModel[]> {
  try {
    const bounded = AbortSignal.any([signal, AbortSignal.timeout(30_000)]);
    const key = await copilotBearer(oauth, request, bounded);
    return (await copilotModels(key, bounded, request)).map(({ id, name }) => ({ id, name }));
  } catch {
    throw new SetupError("Could not load Copilot chat models for this account. Check your Copilot subscription and organization model policy, then retry. Provider response bodies and credentials are not displayed.");
  }
}

export async function verifySetupProvider(provider: SetupProvider, request: typeof fetch = fetch,
  endpointChecks: Parameters<typeof publicEndpoint>[1] = {}): Promise<void> {
  validateProvider(provider);
  if (!provider.apiKey?.trim()) throw new SetupError(provider.kind === "github-copilot" ? "Sign in with GitHub Copilot first" : "Inference API key is required");
  try {
    const endpoint = provider.endpoint.replace(/\/+$/, "");
    if (provider.kind === "openai-compatible") await publicEndpoint(endpoint, endpointChecks);
    const azure = provider.kind === "foundry" || provider.kind === "azure-openai";
    let key = provider.apiKey;
    const headers: Record<string, string> = { "Content-Type": "application/json" };
    const signal = AbortSignal.timeout(30_000);
    let responses = false;
    if (provider.kind === "github-copilot") {
      Object.assign(headers, copilotHeaders);
      key = await copilotBearer(key, request, signal);
      const model = (await copilotModels(key, signal, request)).find(item => item.id === provider.model);
      if (!model) throw new SetupError("The selected model is no longer available in this Copilot account's enabled model list. Reload models and choose an available model.");
      responses = model.endpoint === "/responses";
    }
    headers[azure ? "api-key" : "Authorization"] = azure ? key : `Bearer ${key}`;
    const url = azure ? `${endpoint}/openai/deployments/${encodeURIComponent(provider.model)}/chat/completions?api-version=2024-12-01-preview`
      : `${endpoint}${responses ? "/responses" : "/chat/completions"}`;
    const messages = [{ role: "user", content: "Reply with only OK." }];
    const result = await responseJSON(await request(url, {
      method: "POST", redirect: "error", signal, headers,
      body: JSON.stringify(responses ? { model: provider.model, input: messages, store: false }
        : { ...(azure ? {} : { model: provider.model }), messages }),
    }));
    const replied = responses
      ? result.status === "completed" && !result.error && Array.isArray(result.output) && result.output.some((item: { type?: unknown; role?: unknown; content?: unknown }) =>
        item?.type === "message" && item.role === "assistant" && Array.isArray(item.content)
        && item.content.some(part => part?.type === "output_text" && typeof part.text === "string" && part.text.trim()))
      : Array.isArray(result.choices) && result.choices.some((choice: any) => typeof choice?.message?.content === "string" && choice.message.content.trim());
    if (!replied) throw new Error("No model reply");
  } catch (error) {
    if (error instanceof SetupError) throw error;
    throw new SetupError("Inference check failed or returned no model reply. Verify connectivity and model access; credentials and response bodies are not displayed.");
  }
}
