// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { COPILOT_TOKEN_URL } from "../../github-copilot.js";
import { discoverSetupCopilotModels, validateProvider, verifySetupProvider } from "./providers.js";
import type { SetupProvider } from "./types.js";

const reply = () => new Response(JSON.stringify({ choices: [{ message: { content: "OK" } }] }));
const copilotCatalog = (id: string, supported_endpoints = ["/chat/completions"]) => new Response(JSON.stringify({
  data: [{ id, capabilities: { type: "chat" }, model_picker_enabled: true, supported_endpoints }],
}));
const hosted: SetupProvider[] = [
  { kind: "openai", endpoint: "https://api.openai.com/v1", model: "gpt-5", apiKey: "openai-fixture" },
  { kind: "anthropic", endpoint: "https://api.anthropic.com/v1", model: "claude-sonnet-4-5", apiKey: "anthropic-fixture" },
  { kind: "openai-compatible", endpoint: "https://models.example.com/v1", model: "org/model:latest", apiKey: "server-fixture" },
];
describe("concierge provider protocols", () => {
  it("discovers only enabled chat-picker models using the exchanged token and bounded official transport", async () => {
    const model = { id: "allowed", name: "Allowed model", capabilities: { type: "chat" }, model_picker_enabled: true };
    const request = vi.fn<typeof fetch>()
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: "short-lived-fixture" })))
      .mockResolvedValueOnce(new Response(JSON.stringify({ data: [model, model,
        { ...model, id: "disabled", policy: { state: "disabled" } }, { ...model, id: "hidden", model_picker_enabled: false },
        { ...model, id: "embedding", capabilities: { type: "embeddings" } },
        { ...model, id: "gpt-responses-fixture", name: "GPT Responses fixture", supported_endpoints: ["/responses"] },
        ...[null, [], "/responses", ["/messages"]].map(supported_endpoints => ({ ...model, id: "unsupported", supported_endpoints })),
        { ...model, id: "<script>" }, { ...model, id: "another", policy: { state: "enabled" }, supported_endpoints: ["/chat/completions"] },
      ] })));
    expect(await discoverSetupCopilotModels("oauth-fixture", new AbortController().signal, request))
      .toEqual([{ id: "allowed", name: "Allowed model" }, { id: "another", name: "Allowed model" }, { id: "gpt-responses-fixture", name: "GPT Responses fixture" }]);
    expect(request.mock.calls[0][0]).toBe(COPILOT_TOKEN_URL);
    expect(request.mock.calls[1][0]).toBe("https://api.githubcopilot.com/models");
    expect(request.mock.calls[1][1]).toMatchObject({ redirect: "error", signal: expect.any(AbortSignal),
      headers: { Authorization: "Bearer short-lived-fixture", "Copilot-Integration-Id": "vscode-chat" } });
    expect(JSON.stringify(request.mock.calls[1])).not.toContain("oauth-fixture");
  });
  it.each([{}, { data: [] }, { data: null }, { data: "private-provider-body" }])("fails visibly on unusable catalogs", async catalog => {
    const request = vi.fn<typeof fetch>()
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: "private-exchanged-token" })))
      .mockResolvedValueOnce(new Response(JSON.stringify(catalog)));
    const outcome = await discoverSetupCopilotModels("private-oauth", new AbortController().signal, request).catch(error => error);
    expect(outcome.message).toContain("Could not load Copilot chat models");
    expect(outcome.message).not.toMatch(/private-/);
  });
  it("cancels catalog transport and rejects oversized responses without revealing them", async () => {
    const controller = new AbortController(); controller.abort();
    const request = vi.fn<typeof fetch>().mockImplementation(async (_url, options) => {
      expect(options?.signal?.aborted).toBe(true); throw new Error("private-body");
    });
    await expect(discoverSetupCopilotModels("oauth", controller.signal, request)).rejects.toThrow("Could not load");
    const oversized = vi.fn<typeof fetch>().mockResolvedValueOnce(new Response(JSON.stringify({ token: "fixture" })))
      .mockResolvedValueOnce(new Response("s".repeat(1024 * 1024 + 1)));
    await expect(discoverSetupCopilotModels("oauth", new AbortController().signal, oversized)).rejects.toThrow("Could not load");
  });
  it.each(hosted)("probes $kind using its actual shared-runtime Chat Completions route", async provider => {
    const request = vi.fn(async () => reply());
    await verifySetupProvider(provider, request as typeof fetch, { resolveAddresses: async () => ["8.8.8.8"] });
    expect(request).toHaveBeenCalledExactlyOnceWith(`${provider.endpoint}/chat/completions`, expect.objectContaining({
      redirect: "error", signal: expect.any(AbortSignal), headers: { "Content-Type": "application/json", Authorization: `Bearer ${provider.apiKey}` },
      body: JSON.stringify({ model: provider.model, messages: [{ role: "user", content: "Reply with only OK." }] }),
    }));
  });
  it.each([{ endpoints: ["/chat/completions"] }, { endpoints: ["/responses", "/chat/completions"] }])("preserves Copilot Chat Completions when advertised: $endpoints", async ({ endpoints }) => {
    const request = vi.fn<typeof fetch>().mockResolvedValueOnce(new Response(JSON.stringify({ token: "short-lived-fixture", chat_enabled: true })))
      .mockResolvedValueOnce(copilotCatalog("gpt-5", endpoints)).mockResolvedValueOnce(reply());
    await verifySetupProvider({ kind: "github-copilot", endpoint: "https://api.githubcopilot.com", model: "gpt-5", apiKey: "oauth-fixture" }, request);
    expect(request.mock.calls[0][0]).toBe(COPILOT_TOKEN_URL);
    expect(request.mock.calls[0][1]?.headers).toMatchObject({ Authorization: "token oauth-fixture" });
    expect(request.mock.calls[2][0]).toBe("https://api.githubcopilot.com/chat/completions");
    expect(request.mock.calls[2][1]?.headers).toMatchObject({ Authorization: "Bearer short-lived-fixture", "Copilot-Integration-Id": "vscode-chat" });
    expect(JSON.stringify(request.mock.calls.slice(1))).not.toContain("oauth-fixture");
    expect(request).toHaveBeenCalledTimes(3);
  });
  it("probes Responses-only GPT models directly without submitting a failed chat request first", async () => {
    const request = vi.fn<typeof fetch>()
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: "short-lived-fixture" })))
      .mockResolvedValueOnce(copilotCatalog("gpt-responses-fixture", ["/responses"]))
      .mockResolvedValueOnce(new Response(JSON.stringify({ status: "completed", output: [
        { type: "message", role: "assistant", content: [{ type: "output_text", text: "OK" }] },
      ] })));
    await verifySetupProvider({ kind: "github-copilot", endpoint: "https://api.githubcopilot.com", model: "gpt-responses-fixture", apiKey: "oauth-fixture" }, request);
    expect(request.mock.calls[2]).toEqual(["https://api.githubcopilot.com/responses", expect.objectContaining({
      method: "POST", redirect: "error", signal: expect.any(AbortSignal),
      headers: expect.objectContaining({ Authorization: "Bearer short-lived-fixture" }),
      body: JSON.stringify({ model: "gpt-responses-fixture", input: [{ role: "user", content: "Reply with only OK." }], store: false }),
    })]);
    expect(request).toHaveBeenCalledTimes(3);
    expect(JSON.stringify(request.mock.calls.slice(1))).not.toContain("oauth-fixture");
  });
  it.each([
    { status: "failed", error: { message: "private-provider-error" }, output: [] },
    { status: "incomplete", output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "partial" }] }] },
    { status: "completed", output: [{ type: "function_call", name: "not-a-reply" }] },
    { status: "completed", output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: " " }] }] },
  ])("does not treat an unsuccessful or empty Responses result as a model reply: %j", async response => {
    const request = vi.fn<typeof fetch>()
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: "fixture" })))
      .mockResolvedValueOnce(copilotCatalog("gpt-responses-fixture", ["/responses"]))
      .mockResolvedValueOnce(new Response(JSON.stringify(response)));
    await expect(verifySetupProvider({ kind: "github-copilot", endpoint: "https://api.githubcopilot.com", model: "gpt-responses-fixture", apiKey: "fixture" }, request)).rejects.toThrow("no model reply");
    expect(request).toHaveBeenCalledTimes(3);
  });
  it("rechecks current model availability before sending inference", async () => {
    const request = vi.fn<typeof fetch>()
      .mockResolvedValueOnce(new Response(JSON.stringify({ token: "fixture" })))
      .mockResolvedValueOnce(copilotCatalog("different-enabled-model"));
    await expect(verifySetupProvider({ kind: "github-copilot", endpoint: "https://api.githubcopilot.com", model: "removed-model", apiKey: "fixture" }, request)).rejects.toThrow("no longer available");
    expect(request).toHaveBeenCalledTimes(2);
  });
  it("rejects disabled Copilot chat without making an inference call", async () => {
    const request = vi.fn(async () => new Response(JSON.stringify({ token: "fixture", chat_enabled: false })));
    await expect(verifySetupProvider({ kind: "github-copilot", endpoint: "https://api.githubcopilot.com", model: "gpt-5", apiKey: "fixture" }, request as typeof fetch)).rejects.toThrow("no model reply");
    expect(request).toHaveBeenCalledOnce();
  });
  it.each(["https://api.openai.com.evil.example/v1", "https://api.openai.com/v1/chat/completions", "https://api.openai.com/v1?key=secret"])("never sends hosted credentials to %s", async endpoint => {
    const request = vi.fn();
    await expect(verifySetupProvider({ ...hosted[0], endpoint }, request)).rejects.toThrow();
    expect(request).not.toHaveBeenCalled();
  });
  it.each(["http://model.models.svc:8000/v1", "https://localhost/v1", "https://127.0.0.1/v1", "https://[::1]/v1", "https://model.models.svc/v1", "https://host.internal/v1"])("does not pretend %s is a qualified local endpoint", endpoint => {
    expect(() => validateProvider({ ...hosted[2], endpoint })).toThrow();
  });
  it("rejects private DNS answers before sending custom endpoint credentials", async () => {
    const request = vi.fn();
    await expect(verifySetupProvider(hosted[2], request, { resolveAddresses: async () => ["127.0.0.1"] })).rejects.toThrow("Inference check failed");
    expect(request).not.toHaveBeenCalled();
  });
  it("bounds and redacts provider responses", async () => {
    await expect(verifySetupProvider(hosted[0], vi.fn(async () => new Response("secret".repeat(12000))) as typeof fetch)).rejects.toThrow("response bodies are not displayed");
  });
});
