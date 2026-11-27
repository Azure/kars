// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import { copilotDeviceLogin, GITHUB_DEVICE_CODE_URL, GITHUB_OAUTH_TOKEN_URL } from "../../github-copilot.js";
import { SetupCopilotLogin } from "./copilot.js";

const info = { userCode: "ABCD-EFGH", verificationUri: "https://github.com/login/device", expiresInSec: 900 };
afterEach(() => vi.useRealTimers());
describe("installer-owned Copilot sign-in", () => {
  it("exposes only a device code/status and never the OAuth token", async () => {
    const login = new SetupCopilotLogin(async prompt => { await prompt(info); return "private-oauth"; }, async () => [{ id: "enabled-chat", name: "Enabled chat" }]);
    login.start(); await vi.waitFor(() => expect(login.state.phase).toBe("connected"));
    expect(login.credential("enabled-chat")).toBe("private-oauth");
    expect(() => login.credential("unlisted")).toThrow("Choose a model");
    expect(JSON.stringify(login.state)).not.toContain("private-oauth");
    expect(login.state.userCode).toBeUndefined();
    login.cancel(); expect(() => login.credential()).toThrow("Sign in");
  });
  it("cancels an old account's catalog and keeps failed discovery unavailable until retry", async () => {
    let finish!: (models: { id: string; name: string }[]) => void;
    const discover = vi.fn()
      .mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }))
      .mockRejectedValueOnce(new Error("private-oauth provider-body"))
      .mockResolvedValueOnce([{ id: "current", name: "Current model" }]);
    const login = new SetupCopilotLogin(async () => "private-oauth", discover);
    login.start(); await vi.waitFor(() => expect(discover).toHaveBeenCalledOnce());
    login.start(); await vi.waitFor(() => expect(login.state.modelsPhase).toBe("failed"));
    expect(discover.mock.calls[0][1].aborted).toBe(true);
    finish([{ id: "old-account", name: "Old model" }]); await Promise.resolve();
    expect(login.state.models).toBeUndefined();
    expect(JSON.stringify(login.state)).not.toMatch(/private-oauth|provider-body|old-account/);
    expect(() => login.credential("current")).toThrow("Choose a model");
    login.refreshModels(); await vi.waitFor(() => expect(login.state.modelsPhase).toBe("ready"));
    expect(login.credential("current")).toBe("private-oauth"); login.cancel();
  });
  it("cancels pending authorization and discards late credentials", async () => {
    let finish!: (token: string) => void, signal: AbortSignal | undefined;
    const login = new SetupCopilotLogin(async (prompt, options) => {
      signal = options?.signal; await prompt(info);
      return new Promise(resolve => { finish = resolve; });
    });
    login.start(); await vi.waitFor(() => expect(finish).toBeDefined());
    expect(login.state.phase).toBe("pending"); login.cancel();
    expect(signal?.aborted).toBe(true); finish("late-secret"); await Promise.resolve();
    expect(login.state.phase).toBe("idle"); expect(() => login.credential()).toThrow();
  });
  it("redacts provider errors", async () => {
    const login = new SetupCopilotLogin(async () => { throw new Error("private-oauth provider-body"); });
    login.start(); await vi.waitFor(() => expect(login.state.phase).toBe("failed"));
    expect(JSON.stringify(login.state)).not.toMatch(/private-oauth|provider-body/);
  });
  it("reuses the real device flow with deadlines, no redirects and abortable polling", async () => {
    const request = vi.fn<typeof fetch>().mockResolvedValueOnce(new Response(JSON.stringify({ device_code: "private-device", user_code: info.userCode, verification_uri: info.verificationUri, expires_in: 900, interval: 5 })))
      .mockResolvedValueOnce(new Response(JSON.stringify({ access_token: "oauth-fixture" })));
    const prompt = vi.fn();
    const operation = copilotDeviceLogin(prompt, { request });
    expect(await operation).toBe("oauth-fixture");
    expect(request.mock.calls.map(call => call[0])).toEqual([GITHUB_DEVICE_CODE_URL, GITHUB_OAUTH_TOKEN_URL]);
    for (const call of request.mock.calls) expect(call[1]).toMatchObject({ redirect: "error", signal: expect.any(AbortSignal) });
    expect(prompt).toHaveBeenCalledExactlyOnceWith(info);
  }, 10_000);
  it("never displays an arbitrary verification URL from an unexpected response", async () => {
    const prompt = vi.fn();
    const request = vi.fn(async () => new Response(JSON.stringify({ device_code: "fixture", user_code: info.userCode, verification_uri: "https://evil.example", expires_in: 900 })));
    await expect(copilotDeviceLogin(prompt, { request: request as typeof fetch })).rejects.toThrow("Invalid GitHub");
    expect(prompt).not.toHaveBeenCalled();
  });
});
