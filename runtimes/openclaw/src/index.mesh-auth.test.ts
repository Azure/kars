// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createServer, type ServerResponse } from "node:http";
import type { EventEmitter } from "node:events";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  generate: vi.fn(), transport: vi.fn(), binding: vi.fn(), mission: vi.fn(),
  frames: [] as any[], missionOptions: undefined as any,
}));
vi.mock("@kars/mesh", () => ({ generateIdentity: mocks.generate, createMeshTransport: mocks.transport }));
vi.mock("ws", () => ({ default: class {
  send(value: string) { mocks.frames.push(JSON.parse(value)); }
  emit() { return true; }
} }));
vi.mock("./core/mission-bootstrap.js", () => ({ initializeMissionBinding: mocks.binding }));
vi.mock("./core/foundry-discovery.js", () => ({ discoverFoundryProject: async () => null }));
vi.mock("./core/mission-receiver.js", () => ({ MissionReceiver: class {
  constructor(options: any) { mocks.missionOptions = options; }
  handle(...args: any[]) { return mocks.mission(...args); }
} }));

const did = `did:mesh:${"a".repeat(32)}`;
const identity = { amid: did, did, signingPublicKey: "public", signingPrivateKey: "private" };
const owner = process as any;
const lifecycleEmitter: EventEmitter = process;
const symbolNames = ["agt-mesh-client", "agt-identity", "agt-init-lock", "agt-init-promise", "agt-required-auth"];
let server: ReturnType<typeof createServer>;
let raw: any;
let messages: Array<(...args: any[]) => any>;
let disconnects: Array<() => void>;
let logs: string[];
let exit: ReturnType<typeof vi.spyOn>;
let mintStatus: number;
let mintBody: any;
let verifyBody: any;
let holdVerify: boolean;
let verification: ServerResponse | undefined;
let requests: Array<{ path: string; body: any }>;
const lifecycleEvents = ["SIGTERM", "SIGINT", "unhandledRejection"] as const;
let baselineListeners: Map<typeof lifecycleEvents[number], Set<any>>;
let timers: Set<ReturnType<typeof setTimeout>>;

beforeEach(async () => {
  vi.resetModules();
  vi.clearAllMocks();
  baselineListeners = new Map(lifecycleEvents.map(event => [event, new Set(lifecycleEmitter.listeners(event))]));
  for (const name of symbolNames) expect(owner[Symbol.for(name)]).toBeUndefined();
  vi.stubEnv("MESH_AUTH_BACKEND", "EntraAgentIdentity");
  for (const key of ["AGT_SKIP_INIT", "AGT_SKIP_ENTRA", "AGT_SKIP_VERIFY", "AGT_TRUSTED_PEERS", "OFFLOAD_REQUEST_ID"]) vi.stubEnv(key, "");
  vi.stubEnv("AGT_OAUTH_TOKEN", "secret-stale-env-token");
  vi.stubEnv("SANDBOX_NAME", "test-agent");
  vi.stubEnv("KARS_PROVIDER", "github-models");
  exit = vi.spyOn(process, "exit").mockImplementation((() => undefined) as never);
  timers = new Set();
  const timeout = global.setTimeout, interval = global.setInterval;
  vi.spyOn(global, "setTimeout").mockImplementation(((...args: Parameters<typeof timeout>) => {
    const timer = timeout(...args); timers.add(timer); return timer;
  }) as typeof timeout);
  vi.spyOn(global, "setInterval").mockImplementation(((...args: Parameters<typeof interval>) => {
    const timer = interval(...args); timers.add(timer); return timer;
  }) as typeof interval);
  requests = []; messages = []; disconnects = []; logs = []; mocks.frames.length = 0;
  mintStatus = 200;
  mintBody = { access_token: "secret-fresh-token", token_type: "Bearer", expires_in: 3000 };
  verifyBody = { did, tier: "verified", verified_app_id: "app", verified_tenant_id: "tenant" };
  holdVerify = false; verification = undefined;
  mocks.generate.mockResolvedValue(identity);
  mocks.binding.mockResolvedValue({ identity, target: {}, admission: {}, contract: {} });
  mocks.mission.mockResolvedValue(true);
  raw = {
    isConnected: false, currentDid: did,
    connect: vi.fn(), disconnect: vi.fn(async () => { raw.isConnected = false; }),
    send: vi.fn(), drainInbox: vi.fn(), enableKnockEnforcement: vi.fn(),
    onMessage: (callback: any) => messages.push(callback),
    onDisconnect: (callback: any) => disconnects.push(callback),
    onKnock: vi.fn(), onError: vi.fn(), onE2EVerified: vi.fn(),
  };
  mocks.transport.mockImplementation(async (options: any) => {
    raw.connect.mockImplementation(async () => {
      const socket = options.wsFactory?.("ws://test-relay");
      socket?.send(JSON.stringify({ type: "connect", token: "ignored-sdk-token" }));
      raw.isConnected = true;
    });
    return raw;
  });
  server = createServer((req, res) => {
    let body = "";
    req.on("data", chunk => { body += chunk; });
    req.on("end", () => {
      requests.push({ path: req.url!, body: body ? JSON.parse(body) : undefined });
      if (req.url === "/v1/mesh-token") { res.writeHead(mintStatus); res.end(JSON.stringify(mintBody)); }
      else if (req.url === "/agt/registry/v1/registry/verify") {
        if (holdVerify) verification = res;
        else res.end(JSON.stringify(verifyBody));
      } else { res.writeHead(404); res.end("{}"); }
    });
  });
  await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("Test router unavailable");
  vi.stubEnv("KARS_ROUTER_URL", `http://127.0.0.1:${address.port}`);
});

afterEach(async () => {
  await owner[Symbol.for("agt-required-auth")]?.fail();
  server.closeAllConnections();
  await new Promise<void>(resolve => server.close(() => resolve()));
  for (const timer of timers) { clearTimeout(timer); clearInterval(timer); }
  for (const name of symbolNames) delete owner[Symbol.for(name)];
  for (const [event, previous] of baselineListeners) {
    for (const listener of lifecycleEmitter.listeners(event)) if (!previous.has(listener)) {
      lifecycleEmitter.removeListener(event, listener as (...args: unknown[]) => void);
    }
  }
  vi.unstubAllEnvs();
  vi.restoreAllMocks();
});

async function register() {
  const plugin = (await import("./index.js")).default;
  const logger = { info: (s: string) => logs.push(s), warn: (s: string) => logs.push(s) };
  plugin.register({ config: {}, pluginConfig: {}, logger, registerTool: vi.fn(), registerCommand: vi.fn(), registerProvider: vi.fn(), registerCli: vi.fn() });
}
async function initialized() { await owner[Symbol.for("agt-init-promise")]; }
function publicClient() { return owner[Symbol.for("agt-mesh-client")]; }

describe("OpenClaw required Entra plugin wiring", () => {
  it("keeps the singleton candidate and mission handler private until current-token verification", async () => {
    holdVerify = true;
    await register();
    await vi.waitFor(() => expect(verification).toBeDefined());
    expect(publicClient()).toBeUndefined();
    expect(owner[Symbol.for("agt-identity")]).toBeUndefined();
    expect(owner[Symbol.for("agt-init-lock")]).toBe(true);
    await messages[0](did, { type: "mission:probe" }, "encrypted");
    expect(mocks.mission).not.toHaveBeenCalled();
    expect(() => mocks.missionOptions.execute("work")).toThrow(/authentication unavailable/);
    await register();
    expect(mocks.transport).toHaveBeenCalledOnce();
    verification!.end(JSON.stringify(verifyBody));
    await initialized();
    expect(publicClient().isConnected).toBe(true);
    expect(publicClient()).not.toBe(raw);
    expect(mocks.frames).toEqual([{ type: "connect", token: "secret-fresh-token" }]);
    expect(requests.find(request => request.body)?.body).toEqual({ amid: did, verification_token: "secret-fresh-token" });
    await messages[0](did, { type: "mission:probe" }, "encrypted");
    expect(mocks.mission).toHaveBeenCalledOnce();
    expect(logs.join(" ")).not.toContain("secret-");
    expect(exit).not.toHaveBeenCalled();
  });

  it.each(["missing", "mint", "verification"])("fails closed on %s failure and retains singleton custody", async failure => {
    if (failure === "missing") mintBody = {};
    if (failure === "mint") mintStatus = 503;
    if (failure === "verification") verifyBody = { ...verifyBody, success: false };
    await register(); await initialized();
    expect(publicClient()).toBeUndefined();
    expect(owner[Symbol.for("agt-identity")]).toBeUndefined();
    expect(owner[Symbol.for("agt-init-lock")]).toBe(true);
    expect(raw.disconnect).toHaveBeenCalledOnce();
    expect(exit).toHaveBeenCalledWith(1);
    expect(logs.join(" ")).not.toMatch(/secret-|anonymous development mode permitted/);
  });

  it("revokes cached client and mission readiness on reconnect until a fresh token is verified", async () => {
    await register(); await initialized();
    const cached = publicClient();
    mintBody = { ...mintBody, access_token: "secret-reconnect-token" };
    holdVerify = true;
    raw.isConnected = false;
    disconnects.forEach(callback => callback());
    expect(publicClient()).toBeUndefined();
    expect(cached.isConnected).toBe(false);
    await messages[0](did, { type: "mission:probe" }, "encrypted");
    expect(mocks.mission).not.toHaveBeenCalled();
    await vi.waitFor(() => expect(verification).toBeDefined());
    await register();
    expect(exit).not.toHaveBeenCalled();
    expect(mocks.transport).toHaveBeenCalledOnce();
    verification!.end(JSON.stringify(verifyBody));
    await owner[Symbol.for("agt-required-auth")].connect();
    expect(publicClient()).toBe(cached);
    expect(cached.isConnected).toBe(true);
    expect(mocks.frames.at(-1).token).toBe("secret-reconnect-token");
    expect(exit).not.toHaveBeenCalled();
  });

  it.each(["AGT_SKIP_INIT", "AGT_SKIP_ENTRA", "AGT_SKIP_VERIFY"])("refuses %s before creating a transport", async flag => {
    vi.stubEnv(flag, "1");
    await register();
    expect(mocks.transport).not.toHaveBeenCalled();
    expect(exit).toHaveBeenCalledWith(1);
    expect(publicClient()).toBeUndefined();
  });

  it("retains optional OAuth best-effort verification and environment-token relay behavior", async () => {
    vi.stubEnv("MESH_AUTH_BACKEND", "");
    verifyBody = { success: false };
    await register(); await initialized();
    expect(publicClient()).toBe(raw);
    expect(publicClient().isConnected).toBe(true);
    expect(requests.some(request => request.path === "/v1/mesh-token")).toBe(false);
    expect(mocks.frames[0].token).toBe("ignored-sdk-token");
    expect(requests.find(request => request.body)?.body.verification_token).toBe("secret-stale-env-token");
    expect(logs.join(" ")).toContain("anonymous development mode permitted");
    expect(exit).not.toHaveBeenCalled();
  });
});
