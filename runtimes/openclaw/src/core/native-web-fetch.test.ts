// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { beforeEach, describe, expect, it, vi } from "vitest";
import type { NativeMissionApi, NativeWebFetchProvider } from "./native-mission-api.js";
import { registerNativeWebFetch, type NativeFetchAuthority } from "./native-web-fetch.js";

vi.mock("openclaw/plugin-sdk/web-content-extractor", () => ({
  extractBasicHtmlContent: vi.fn(async () => ({ text: "SDK extracted content", title: "SDK title" })),
  markdownToText: vi.fn((value: string) => value.replaceAll("**", "")),
  stripInvisibleUnicode: vi.fn((value: string) => value),
}));

function harness() {
  const controller = new AbortController();
  const assertCurrent = vi.fn(() => controller.signal.throwIfAborted());
  const claim = vi.fn((_args: Record<string, unknown>, _signal?: AbortSignal): NativeFetchAuthority => ({ signal: controller.signal, assertCurrent }));
  const fetch = vi.fn(async (_args: Record<string, unknown>, _signal: AbortSignal, _native: boolean): Promise<Record<string, unknown>> => ({
    status: 200, headers: { "Content-Type": "text/html; charset=utf-8" }, body: "<html>Source</html>", truncated: false,
  }));
  let provider: NativeWebFetchProvider;
  const api = { id: "kars", registerWebFetchProvider: (value: NativeWebFetchProvider) => { provider = value; } };
  registerNativeWebFetch(api as NativeMissionApi, claim, fetch);
  const config = { plugins: { entries: { kars: { enabled: true, config: { model: "retained" } } } } };
  const tool = provider!.createTool({ config })!;
  return { provider: provider!, config, tool, claim, fetch, controller, assertCurrent };
}
const args = { url: "https://vendor.example/pricing", extractMode: "markdown", maxChars: 12000 };

beforeEach(() => vi.clearAllMocks());

describe("native readable provider over mediated router fetch", () => {
  it("supports late-bound host configuration without treating config as authority or taking credentials", () => {
    const h = harness();
    expect(h.provider.createTool({ config: {} })).not.toBeNull();
    expect(h.config.plugins.entries.kars).toEqual({ enabled: true, config: { model: "retained" } });
    expect(h.provider.requiresCredential).toBe(false);
    expect(h.provider.getCredentialValue()).toBeUndefined();
    expect(() => h.provider.setCredentialValue({}, "secret")).toThrow("does not accept");
  });
  it("claims the authorized invocation before using the router and public native extractor", async () => {
    const h = harness();
    const signal = new AbortController().signal;
    const result = await h.tool.execute(args, { signal });
    expect(h.claim).toHaveBeenCalledWith(args, signal);
    expect(h.claim.mock.invocationCallOrder[0]).toBeLessThan(h.fetch.mock.invocationCallOrder[0]);
    expect(h.fetch).toHaveBeenCalledWith(expect.objectContaining({ url: args.url, method: "GET" }), h.controller.signal, true);
    expect(result).toMatchObject({ text: "SDK extracted content", title: "SDK title", status: 200, finalUrl: args.url,
      contentType: "text/html", extractor: "kars-router-html", truncated: false });
  });
  it("does not fetch without current invocation authority", async () => {
    const h = harness();
    h.claim.mockImplementation(() => { throw new Error("unadmitted"); });
    await expect(h.tool.execute(args)).rejects.toThrow("unadmitted");
    expect(h.fetch).not.toHaveBeenCalled();
  });
  it("rechecks each relative redirect through the router without forwarding source headers", async () => {
    const h = harness();
    h.fetch.mockResolvedValueOnce({ status: 302, headers: { Location: "/plans", "Set-Cookie": "untrusted" }, body: "", truncated: false });
    const result = await h.tool.execute(args);
    expect(h.fetch).toHaveBeenCalledTimes(2);
    expect(h.fetch.mock.calls[1][0]).toEqual({ url: "https://vendor.example/plans", method: "GET", headers: { Accept: expect.any(String) } });
    expect(result.finalUrl).toBe("https://vendor.example/plans");
  });
  it("carries admitted telemetry through every redirect without taking identity from arguments", async () => {
    const h = harness();
    const telemetry = { scope_id: "router-scope", call_id: "native-call-1" };
    h.claim.mockReturnValue({ signal: h.controller.signal, assertCurrent: h.assertCurrent, telemetry });
    h.fetch.mockResolvedValueOnce({ status: 302, headers: { location: "https://other.example/plans" }, body: "", truncated: false });
    await h.tool.execute({ ...args, telemetry: { scope_id: "forged", call_id: "wrong" } });
    expect(h.fetch).toHaveBeenCalledTimes(2);
    for (const [request] of h.fetch.mock.calls) expect(request.telemetry).toEqual(telemetry);
  });
  it("propagates router rejection of a redirect destination without fallback or direct network", async () => {
    const h = harness();
    h.fetch.mockResolvedValueOnce({ status: 302, headers: { location: "https://denied.example/" }, body: "", truncated: false });
    h.fetch.mockRejectedValueOnce(new Error("Router fetch HTTP 403"));
    await expect(h.tool.execute(args)).rejects.toThrow("Router fetch HTTP 403");
    expect(h.fetch).toHaveBeenCalledTimes(2);
  });
  it.each(["http://vendor.example/", "https://user:secret@vendor.example/", "file:///etc/passwd"])("rejects unsafe redirect %s", async target => {
    const h = harness();
    h.fetch.mockResolvedValueOnce({ status: 302, headers: { location: target }, body: "", truncated: false });
    await expect(h.tool.execute(args)).rejects.toThrow("credential-free HTTPS");
    expect(h.fetch).toHaveBeenCalledTimes(1);
  });
  it("bounds redirect chains and detects loops", async () => {
    const h = harness();
    h.fetch.mockResolvedValue({ status: 302, headers: { location: args.url }, body: "", truncated: false });
    await expect(h.tool.execute(args)).rejects.toThrow("redirect loop");
    expect(h.fetch).toHaveBeenCalledTimes(1);
    h.fetch.mockClear().mockImplementation(async () => ({ status: 302, headers: { location: `/hop-${h.fetch.mock.calls.length}` }, body: "", truncated: false }));
    await expect(h.tool.execute(args)).rejects.toThrow("exceeded three redirects");
    expect(h.fetch).toHaveBeenCalledTimes(4);
  });
  it("preserves HTTP errors and incomplete-source disclosure instead of reporting success", async () => {
    const h = harness();
    h.fetch.mockResolvedValue({ status: 503, headers: { "content-type": "text/plain" }, body: "Retry later", truncated: true });
    expect(await h.tool.execute(args)).toMatchObject({ status: 503, text: "Retry later", truncated: true, warning: expect.stringContaining("incomplete") });
  });
  it("uses SDK markdown-to-text conversion for text mode", async () => {
    const h = harness();
    h.fetch.mockResolvedValue({ status: 200, headers: { "content-type": "text/markdown" }, body: "**Price**", truncated: false });
    expect(await h.tool.execute({ ...args, extractMode: "text" })).toMatchObject({ text: "Price" });
  });
  it("does not return stale content after cancellation during fetch", async () => {
    const h = harness();
    h.fetch.mockImplementation(async () => { h.controller.abort(new Error("cancelled")); return { status: 200, headers: {}, body: "late", truncated: false }; });
    await expect(h.tool.execute(args)).rejects.toThrow("cancelled");
  });
  it("does not interpret binary content as a readable page", async () => {
    const h = harness();
    h.fetch.mockResolvedValue({ status: 200, headers: { "content-type": "application/pdf" }, body: "%PDF", truncated: false });
    await expect(h.tool.execute(args)).rejects.toThrow("cannot extract application/pdf");
  });
});
