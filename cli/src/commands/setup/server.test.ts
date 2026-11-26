// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { request } from "node:http";
import { afterEach, describe, expect, it, vi } from "vitest";
import { startSetupServer, type SetupService } from "./server.js";
import { SetupError, type SetupState } from "./types.js";

const servers: Awaited<ReturnType<typeof startSetupServer>>[] = [];
afterEach(async () => { for (const server of servers.splice(0)) { server.close(); await server.closed; } });
async function fixture() {
  const state: SetupState = { phase: "draft", busy: false, contexts: [], checks: [], steps: [], providerConfigured: false };
  const backend: SetupService = { state, initialize: vi.fn(async () => {}), preflight: vi.fn(async () => state), install: vi.fn(async () => { throw new SetupError("Not approved"); }) };
  const server = await startSetupServer(backend); servers.push(server);
  const post = (url: string, value: unknown, headers: Record<string, string> = {}) => fetch(server.origin + url, { method: "POST", headers: { Origin: server.origin, "Content-Type": "application/json", ...headers }, body: JSON.stringify(value) });
  const login = async () => {
    const response = await post("/api/session", { token: new URLSearchParams(new URL(server.url).hash.slice(1)).get("token") });
    const cookie = response.headers.get("set-cookie")!;
    const { csrfToken } = await response.json() as { csrfToken: string };
    return { cookie, "X-CSRF-Token": csrfToken };
  };
  return { backend, server, post, login };
}

describe("private loopback setup server", () => {
  it("protects destructive cleanup with session, origin and CSRF and admits authorization asynchronously", async () => {
    const { backend, post, login } = await fixture();
    let finish!: () => void;
    backend.cleanup = vi.fn(async input => {
      if (!(input as { confirm?: boolean }).confirm) throw new SetupError("Explicit confirmation required");
      backend.state.busy = true;
      await new Promise<void>(resolve => { finish = resolve; });
      backend.state.busy = false;
    });
    const payload = { clusterName: "unit-cleanup", confirm: true };
    expect((await post("/api/cleanup", payload)).status).toBe(401);
    const auth = await login();
    expect((await post("/api/cleanup", payload, { cookie: auth.cookie })).status).toBe(403);
    expect((await post("/api/cleanup", payload, { ...auth, Origin: "https://evil.example" })).status).toBe(403);
    expect(backend.cleanup).not.toHaveBeenCalled();
    expect((await post("/api/cleanup", {}, auth)).status).toBe(400);
    expect((await post("/api/cleanup", payload, auth)).status).toBe(202);
    expect((await post("/api/shutdown", {}, auth)).status).toBe(400);
    finish();
    await vi.waitFor(() => expect(backend.state.busy).toBe(false));
  });
  it("protects credential-free prerequisite checks with session, origin and CSRF", async () => {
    const { backend, post, login } = await fixture();
    backend.prerequisites = vi.fn(async () => backend.state);
    expect((await post("/api/prerequisites", {})).status).toBe(401);
    const auth = await login();
    expect((await post("/api/prerequisites", {}, { cookie: auth.cookie })).status).toBe(403);
    expect((await post("/api/prerequisites", {}, { ...auth, Origin: "https://evil.example" })).status).toBe(403);
    expect(backend.prerequisites).not.toHaveBeenCalled();
    const selection = { bridge: false, ports: { https: 9443, bridge: 3000, headlamp: 4566 } };
    expect((await post("/api/prerequisites", selection, auth)).status).toBe(200);
    expect(backend.prerequisites).toHaveBeenCalledOnce();
    expect(backend.prerequisites).toHaveBeenCalledWith(selection);
  });
  it("serves self-contained HTML with restrictive CSP and no token in the document", async () => {
    const { server } = await fixture();
    const response = await fetch(server.origin);
    expect(response.status).toBe(200);
    expect(response.headers.get("content-security-policy")).toContain("script-src 'sha256-");
    expect(response.headers.get("content-security-policy")).not.toContain("unsafe-inline");
    expect(response.headers.get("content-security-policy")).toContain("frame-ancestors 'none'");
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(await response.text()).not.toContain(new URL(server.url).hash.slice(1));
  });
  it("opens an external top-level link without consuming its private fragment or invoking setup", async () => {
    const { server, login, backend } = await fixture();
    const response = await new Promise<{ status?: number; text: string }>((resolve, reject) => {
      const req = request(server.origin, { headers: { "Sec-Fetch-Site": "cross-site", "Sec-Fetch-Mode": "navigate", "Sec-Fetch-Dest": "document" } }, response => {
        let text = ""; response.setEncoding("utf8"); response.on("data", chunk => { text += chunk; });
        response.on("end", () => resolve({ status: response.statusCode, text }));
      });
      req.on("error", reject); req.end();
    });
    expect(response.status).toBe(200); expect(response.text).toContain("<html");
    expect(response.text).not.toContain(new URL(server.url).hash.slice(1));
    expect(backend.preflight).not.toHaveBeenCalled(); expect(backend.install).not.toHaveBeenCalled();
    expect((await login()).cookie).toContain("HttpOnly; SameSite=Strict");
  });
  it.each([
    ["GET", "/", "navigate", "iframe"], ["GET", "/", "cors", "document"],
    ["GET", "/api/state", "navigate", "document"], ["POST", "/api/session", "navigate", "document"],
    ["POST", "/api/preflight", "navigate", "document"], ["GET", "/?redirect=1", "navigate", "document"],
  ])("rejects cross-site %s %s (%s/%s) even with valid session credentials", async (method, route, mode, dest) => {
    const { server, login, backend } = await fixture(); const auth = await login();
    const status = await new Promise<number | undefined>((resolve, reject) => {
      const req = request(server.origin + route, { method, headers: { ...auth, Origin: server.origin,
        "Sec-Fetch-Site": "cross-site", "Sec-Fetch-Mode": mode, "Sec-Fetch-Dest": dest } }, response => {
        response.resume(); response.on("end", () => resolve(response.statusCode));
      });
      req.on("error", reject); req.end();
    });
    expect(status).toBe(403); expect(backend.preflight).not.toHaveBeenCalled(); expect(backend.install).not.toHaveBeenCalled();
  });
  it("exchanges a one-time fragment token for an HttpOnly strict cookie", async () => {
    const { server, login } = await fixture();
    const auth = await login();
    expect(auth.cookie).toContain("HttpOnly; SameSite=Strict");
    const state = await fetch(server.origin + "/api/state", { headers: { cookie: auth.cookie } });
    expect(state.status).toBe(200);
    expect(await state.json()).toMatchObject({ csrfToken: auth["X-CSRF-Token"] });
    expect((await login()).cookie).toBeNull();
  });
  it("rejects requests without a session, wrong origin, rebinding host and cross-site fetches", async () => {
    const { server, post, backend } = await fixture();
    expect((await fetch(server.origin + "/api/state")).status).toBe(401);
    expect((await post("/api/preflight", {}, { Origin: "https://example.com" })).status).toBe(403);
    // Node fetch controls Host itself; send an actual hostile Host over HTTP.
    const rebound = await new Promise<number | undefined>((resolve, reject) => {
      const req = request(server.origin, { headers: { Host: "attacker.example" } }, response => {
        response.resume(); response.on("end", () => resolve(response.statusCode));
      });
      req.on("error", reject); req.end();
    });
    expect(rebound).toBe(403);
    expect((await fetch(server.origin, { headers: { "Sec-Fetch-Site": "cross-site" } })).status).toBe(403);
    expect(backend.preflight).not.toHaveBeenCalled();
  });
  it.each(["start", "cancel", "models"])("protects Copilot %s with the same session, origin and CSRF checks", async action => {
    const { login, post, backend, server } = await fixture();
    const method = vi.fn(() => backend.state);
    backend.startCopilot = method; backend.cancelCopilot = method; backend.refreshCopilotModels = method;
    backend.dispose = vi.fn();
    const path = `/api/copilot/${action}`;
    expect((await post(path, {})).status).toBe(401);
    const auth = await login();
    expect((await post(path, {}, { cookie: auth.cookie })).status).toBe(403);
    expect((await post(path, {}, { ...auth, Origin: "https://evil.example" })).status).toBe(403);
    expect(method).not.toHaveBeenCalled();
    expect((await post(path, {}, auth)).status).toBe(200);
    expect(method).toHaveBeenCalledOnce();
    server.close(); await server.closed;
    expect(backend.dispose).toHaveBeenCalledOnce();
  });
  it("requires CSRF on every authenticated mutation", async () => {
    const { login, post, backend } = await fixture();
    const auth = await login();
    expect((await post("/api/preflight", {}, { cookie: auth.cookie })).status).toBe(403);
    expect((await post("/api/preflight", {}, auth)).status).toBe(200);
    expect(backend.preflight).toHaveBeenCalledOnce();
  });
  it("rejects oversized and malformed bodies before invoking backend", async () => {
    const { login, post, backend, server } = await fixture(); const auth = await login();
    expect((await post("/api/preflight", { data: "x".repeat(17_000) }, auth)).status).toBe(400);
    expect((await fetch(server.origin + "/api/preflight", { method: "POST", headers: { ...auth, Origin: server.origin, "Content-Type": "application/json" }, body: "{" })).status).toBe(400);
    expect(backend.preflight).not.toHaveBeenCalled();
  });
  it("returns rejected installation admission instead of claiming it started", async () => {
    const { login, post } = await fixture();
    const response = await post("/api/install", {}, await login());
    expect(response.status).toBe(400); expect(await response.json()).toEqual({ error: "Not approved" });
  });
  it("exposes running state without waiting for the long-running installation", async () => {
    const { login, post, backend } = await fixture();
    let finish!: () => void;
    backend.install = vi.fn(async () => { backend.state.busy = true; backend.state.phase = "installing"; await new Promise<void>(resolve => { finish = resolve; }); });
    const response = await post("/api/install", {}, await login());
    expect(response.status).toBe(202); expect(await response.json()).toMatchObject({ busy: true, phase: "installing" });
    finish();
  });
  it("admits preflight immediately and polls authorization in the same authenticated session", async () => {
    const { login, post, backend, server } = await fixture();
    let finish!: () => void;
    backend.preflight = vi.fn(async () => {
      if (backend.state.busy) throw new SetupError("Another setup operation is running");
      Object.assign(backend.state, { phase: "preflight", busy: true, authorization: "waiting", authorizationTarget: "terminal" });
      await new Promise<void>(resolve => { finish = resolve; });
      Object.assign(backend.state, { phase: "review", busy: false, authorization: "approved" });
      return backend.state;
    });
    const auth = await login();
    const response = await post("/api/preflight", {}, auth);
    expect(response.status).toBe(202);
    expect(await response.json()).toMatchObject({ phase: "preflight", busy: true, authorization: "waiting", authorizationTarget: "terminal" });
    expect((await post("/api/preflight", {}, auth)).status).toBe(400);
    expect(backend.state).toMatchObject({ phase: "preflight", busy: true, authorization: "waiting" });
    expect((await post("/api/shutdown", {}, auth)).status).toBe(400);
    finish();
    const state = await fetch(server.origin + "/api/state", { headers: { cookie: auth.cookie } });
    expect(await state.json()).toMatchObject({ phase: "review", busy: false, authorization: "approved", csrfToken: auth["X-CSRF-Token"] });
    expect(backend.preflight).toHaveBeenCalledTimes(2);
    expect(backend.install).not.toHaveBeenCalled();
  });
  it("does not return command errors or secrets", async () => {
    const { login, post, backend } = await fixture();
    backend.preflight = vi.fn(async () => { throw new Error("api-key=super-secret in stderr"); });
    const response = await post("/api/preflight", {}, await login());
    expect(response.status).toBe(400); expect(await response.text()).not.toContain("super-secret");
  });
  it("refuses shutdown while installing, then closes only the concierge", async () => {
    const { login, post, backend, server } = await fixture(); const auth = await login();
    backend.state.busy = true;
    expect((await post("/api/shutdown", {}, auth)).status).toBe(400);
    backend.state.busy = false;
    expect((await post("/api/shutdown", {}, auth)).status).toBe(200);
    await server.closed; expect(backend.install).not.toHaveBeenCalled();
  });
});
