// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { EventEmitter } from "node:events";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { connectCommand } from "./connect.js";

const { execute, createConnection, createServer } = vi.hoisted(() => ({
  execute: vi.fn(), createConnection: vi.fn(), createServer: vi.fn(),
}));
vi.mock("execa", () => ({ execa: execute }));
vi.mock("node:net", () => ({ createConnection, createServer }));

function forward() {
  let resolve!: (result: { stdout: string }) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<{ stdout: string }>((yes, no) => { resolve = yes; reject = no; });
  const child = Object.assign(promise, {
    stdout: new EventEmitter(), stderr: new EventEmitter(),
    kill: vi.fn(() => { reject(new Error("terminated")); return true; }),
  });
  return {
    child,
    ready: (port = 19119) => child.stdout.emit("data", Buffer.from(`Forwarding from 127.0.0.1:${port} -> 9119\n`)),
    fail: (message: string) => { child.stderr.emit("data", Buffer.from(message)); reject(new Error(message)); },
    finish: () => resolve({ stdout: "" }),
  };
}

let runtime: "Hermes" | "OpenClaw";
let tunnel: ReturnType<typeof forward>;
let onForward: () => void;
let podPhase: string;
let podWaitingReason: string;
let runtimeClass: string;
let secret: string;
let fetchMock: ReturnType<typeof vi.fn>;
let originalExitCode: typeof process.exitCode;
let signalListeners: Record<"SIGINT" | "SIGTERM", NodeJS.SignalsListener[]>;

const run = (...args: string[]) => connectCommand().parseAsync([
  "node", "connect", "sre", "--cloud", "--context", "kind-beta", ...args,
]);
const calls = () => execute.mock.calls.map(([file, args]) => ({ file, args: args as string[] }));
const output = () => vi.mocked(console.log).mock.calls.flat().join("\n");
const errors = () => vi.mocked(console.error).mock.calls.flat().join("\n");
const expectNoExecOrSecret = () => {
  expect(calls().some(({ args }) => args.includes("exec") || args.includes("gateway-token"))).toBe(false);
};
const signal = (name: "SIGINT" | "SIGTERM") => {
  const handler = process.listeners(name).find(listener => !signalListeners[name].includes(listener));
  expect(handler).toBeDefined();
  handler!(name);
};
const expectCleaned = () => {
  expect(tunnel.child.kill).toHaveBeenCalledWith("SIGTERM");
  expect(process.listeners("SIGINT")).toEqual(signalListeners.SIGINT);
  expect(process.listeners("SIGTERM")).toEqual(signalListeners.SIGTERM);
  expect(tunnel.child.stdout.listenerCount("data")).toBe(0);
  expect(tunnel.child.stderr.listenerCount("data")).toBe(0);
};

beforeEach(() => {
  vi.useFakeTimers();
  originalExitCode = process.exitCode;
  process.exitCode = 0;
  signalListeners = { SIGINT: process.listeners("SIGINT"), SIGTERM: process.listeners("SIGTERM") };
  runtime = "Hermes";
  podPhase = "Running";
  podWaitingReason = "CrashLoopBackOff";
  runtimeClass = "";
  secret = Buffer.from("test-gateway-token").toString("base64");
  tunnel = forward();
  onForward = () => tunnel.ready();
  execute.mockReset();
  createConnection.mockReset().mockImplementation(() => {
    const socket = Object.assign(new EventEmitter(), { destroy: vi.fn() });
    queueMicrotask(() => socket.emit("error", new Error("no listener")));
    return socket;
  });
  createServer.mockReset().mockImplementation(() => {
    const server = Object.assign(new EventEmitter(), {
      listen: vi.fn(() => queueMicrotask(() => server.emit("listening"))),
      close: vi.fn((callback: () => void) => callback()),
    });
    return server;
  });
  execute.mockImplementation((file: string, args: string[]) => {
    if (["open", "start", "xdg-open"].includes(file)) return Promise.resolve({ stdout: "" });
    if (file !== "kubectl") throw new Error(`Unexpected command: ${file}`);
    if (args.includes("port-forward")) { queueMicrotask(onForward); return tunnel.child; }
    if (args.includes("karssandbox")) return Promise.resolve({ stdout: JSON.stringify({ spec: { runtime: { kind: runtime } } }) });
    if (args.includes("gateway-token")) return Promise.resolve({ stdout: secret });
    if (args.includes("jsonpath={.items[0].spec.runtimeClassName}")) return Promise.resolve({ stdout: runtimeClass });
    if (args.includes("pod")) return Promise.resolve({ stdout: JSON.stringify({ items: [{ status: {
      phase: podPhase,
      containerStatuses: [{ name: runtime === "Hermes" ? "agent" : "openclaw", ready: podPhase === "Running",
        ...(podPhase !== "Running" ? { state: { waiting: { reason: podWaitingReason } } } : {}),
      }],
    } }] }) });
    if (args.includes("deploy") || args.includes("exec") || args.includes("rollout")) return Promise.resolve({ stdout: "" });
    throw new Error(`Unexpected kubectl args: ${args.join(" ")}`);
  });
  vi.spyOn(console, "log").mockImplementation(() => {});
  vi.spyOn(console, "error").mockImplementation(() => {});
  vi.spyOn(process.stderr, "write").mockImplementation(() => true);
  fetchMock = vi.fn().mockResolvedValue({ ok: true, status: 200, body: { cancel: vi.fn().mockResolvedValue(undefined) } });
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  process.exitCode = originalExitCode;
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("Hermes browser connection", () => {
  it("forwards the dashboard service on the exact loopback port and context, without a gateway token", async () => {
    const running = run("--web", "--port", "19119");
    await vi.waitFor(() => expect(output()).toContain("Port-forward active"));
    expect(calls().find(({ args }) => args.includes("port-forward"))?.args).toEqual([
      "--context", "kind-beta", "port-forward", "--address", "127.0.0.1", "-n", "kars-sre", "svc/sre", "19119:9119",
    ]);
    expect(calls().filter(({ file }) => file === "kubectl").every(({ args }) => args[0] === "--context" && args[1] === "kind-beta")).toBe(true);
    expect(fetchMock).toHaveBeenCalledWith("http://127.0.0.1:19119/", expect.objectContaining({ redirect: "error" }));
    expect(calls().some(({ file, args }) => file !== "kubectl" && args[0] === "http://127.0.0.1:19119/")).toBe(true);
    expectNoExecOrSecret();
    expect(createConnection).not.toHaveBeenCalled();
    expect(createServer).not.toHaveBeenCalled();
    expect(output()).not.toContain("#token=");
    tunnel.finish();
    await running;
    expectCleaned();
  });

  it("retains the default local port and lets --web take precedence over --shell", async () => {
    onForward = () => tunnel.ready(18789);
    const running = run("--web", "--shell");
    await vi.waitFor(() => expect(output()).toContain("http://127.0.0.1:18789/"));
    expect(calls().some(({ args }) => args.includes("18789:9119"))).toBe(true);
    expectNoExecOrSecret();
    tunnel.finish();
    await running;
  });

  it.each(["0", "65536", "19119.5", "abc", "-1"])("rejects invalid --port %s", async port => {
    await run("--web", "--port", port);
    expect(errors()).toContain("--port must be an integer between 1 and 65535");
    expect(calls().some(({ args }) => args.includes("port-forward"))).toBe(false);
    expect(process.exitCode).toBe(1);
  });

  it.each(["address already in use", "Forbidden: pods/portforward", "services sre not found"])("reports early kubectl failure: %s", async message => {
    onForward = () => tunnel.fail(message);
    await run("--web", "--port", "19119");
    expect(errors()).toContain(message);
    expect(output()).not.toContain("Port-forward active");
    expect(fetchMock).not.toHaveBeenCalled();
    expectNoExecOrSecret();
    expect(process.exitCode).toBe(1);
    expectCleaned();
  });

  it("waits for kubectl readiness rather than advertising a tunnel after a fixed delay", async () => {
    onForward = () => {};
    const running = run("--web", "--port", "19119");
    await vi.waitFor(() => expect(calls().some(({ args }) => args.includes("port-forward"))).toBe(true));
    await vi.advanceTimersByTimeAsync(2000);
    expect(output()).not.toContain("Port-forward active");
    expect(fetchMock).not.toHaveBeenCalled();
    tunnel.ready();
    await vi.waitFor(() => expect(output()).toContain("Port-forward active"));
    tunnel.finish();
    await running;
  });

  it("times out startup and terminates only its own tunnel", async () => {
    onForward = () => {};
    const running = run("--web", "--port", "19119");
    await vi.waitFor(() => expect(calls().some(({ args }) => args.includes("port-forward"))).toBe(true));
    await vi.advanceTimersByTimeAsync(30_000);
    await running;
    expect(errors()).toContain("not ready after 30s");
    expect(process.exitCode).toBe(1);
    expectCleaned();
  });

  it.each(["SIGINT", "SIGTERM"] as const)("cleans up %s while waiting for readiness", async name => {
    onForward = () => {};
    const running = run("--web", "--port", "19119");
    await vi.waitFor(() => expect(calls().some(({ args }) => args.includes("port-forward"))).toBe(true));
    signal(name);
    await running;
    expect(output()).not.toContain("Port-forward active");
    expect(process.exitCode).toBe(0);
    expectCleaned();
  });

  it("cleans up an active tunnel on Ctrl+C without exiting the host process", async () => {
    const running = run("--web", "--port", "19119");
    await vi.waitFor(() => expect(output()).toContain("Port-forward active"));
    signal("SIGINT");
    await running;
    expect(process.exitCode).toBe(0);
    expectCleaned();
  });

  it.each(["lost connection to pod", "connection reset by peer", "unexpected EOF"])("reconnects an active tunnel after %s without reopening the browser", async message => {
    const running = run("--web", "--port", "19119");
    await vi.waitFor(() => expect(output()).toContain("Port-forward active"));
    const first = tunnel;
    tunnel = forward();
    first.fail(message);
    await vi.waitFor(() => expect(output()).toContain("Reconnecting on the same port"));
    await vi.advanceTimersByTimeAsync(1000);
    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    const forwards = calls().filter(({ args }) => args.includes("port-forward"));
    expect(forwards).toHaveLength(2);
    expect(forwards[1]).toEqual(forwards[0]);
    expect(calls().filter(({ file }) => file !== "kubectl")).toHaveLength(1);
    expect(first.child.kill).toHaveBeenCalledWith("SIGTERM");
    expect(first.child.stdout.listenerCount("data")).toBe(0);
    expect(first.child.stderr.listenerCount("data")).toBe(0);
    expectNoExecOrSecret();
    tunnel.finish();
    await running;
    expect(process.exitCode).toBe(0);
    expectCleaned();
  });

  it.each(["Hermes", "OpenClaw"] as const)("waits for the replacement %s pod before reconnecting on the same port", async kind => {
    runtime = kind;
    const running = run("--web", "--port", "19119");
    await vi.waitFor(() => expect(output()).toContain("Port-forward active"));
    const first = tunnel;
    tunnel = forward();
    podPhase = "Pending";
    podWaitingReason = "ContainerCreating";
    first.fail("lost connection to pod");
    await vi.waitFor(() => expect(output()).toContain("Reconnecting on the same port"));
    await vi.advanceTimersByTimeAsync(4000);
    expect(calls().filter(({ args }) => args.includes("port-forward"))).toHaveLength(1);
    podPhase = "Running";
    await vi.advanceTimersByTimeAsync(3000);
    await vi.waitFor(() => expect(output().match(/Port-forward active/g)).toHaveLength(2));
    const forwards = calls().filter(({ args }) => args.includes("port-forward"));
    expect(forwards).toHaveLength(2);
    expect(forwards[1]).toEqual(forwards[0]);
    expect(first.child.kill).toHaveBeenCalledWith("SIGTERM");
    tunnel.finish();
    await running;
    expect(process.exitCode).toBe(0);
    expectCleaned();
  });

  it.each(["dashboard transport", "connection refused", "Current status=Pending"])("keeps recovering when the replacement has a transient %s failure", async failure => {
    const running = run("--web", "--port", "19119");
    await vi.waitFor(() => expect(output()).toContain("Port-forward active"));
    const first = tunnel;
    tunnel = forward();
    if (failure === "dashboard transport") {
      fetchMock.mockRejectedValueOnce(new TypeError("fetch failed", { cause: { code: "UND_ERR_SOCKET" } }));
    } else {
      onForward = () => tunnel.fail(failure);
    }
    first.fail("lost connection to pod");
    await vi.waitFor(() => expect(output()).toContain("Reconnecting on the same port"));
    await vi.advanceTimersByTimeAsync(1000);
    await vi.waitFor(() => expect(output().match(/Reconnecting on the same port/g)).toHaveLength(2));
    const second = tunnel;
    tunnel = forward();
    onForward = () => tunnel.ready();
    await vi.advanceTimersByTimeAsync(1000);
    await vi.waitFor(() => expect(output().match(/Port-forward active/g)).toHaveLength(2));
    const forwards = calls().filter(({ args }) => args.includes("port-forward"));
    expect(forwards).toHaveLength(3);
    expect(forwards.every(call => JSON.stringify(call) === JSON.stringify(forwards[0]))).toBe(true);
    expect(calls().filter(({ file }) => file !== "kubectl")).toHaveLength(1);
    expect(second.child.kill).toHaveBeenCalledWith("SIGTERM");
    tunnel.finish();
    await running;
    expect(process.exitCode).toBe(0);
    expectCleaned();
  });

  it("does not reset the recovery deadline on repeated dashboard transport failures", async () => {
    const running = run("--web", "--port", "19119");
    await vi.waitFor(() => expect(output()).toContain("Port-forward active"));
    const first = tunnel;
    const original = execute.getMockImplementation()!;
    execute.mockImplementation((file, args) => {
      if (args.includes("port-forward")) tunnel = forward();
      return original(file, args);
    });
    fetchMock.mockRejectedValue(new TypeError("fetch failed", { cause: { code: "ECONNREFUSED" } }));
    first.fail("lost connection to pod");
    await vi.waitFor(() => expect(output()).toContain("Reconnecting on the same port"));
    await vi.advanceTimersByTimeAsync(181_000);
    await running;
    expect(errors()).toContain("timed out after 3m");
    expect(output().match(/Port-forward active/g)).toHaveLength(1);
    expect(calls().filter(({ args }) => args.includes("port-forward")).length).toBeGreaterThan(1);
    expect(calls().filter(({ file }) => file !== "kubectl")).toHaveLength(1);
    expect(process.exitCode).toBe(1);
    expectCleaned();
  });

  it.each(["SIGINT", "SIGTERM"] as const)("cancels replacement-pod readiness on %s", async name => {
    const running = run("--web", "--port", "19119");
    await vi.waitFor(() => expect(output()).toContain("Port-forward active"));
    podPhase = "Pending";
    podWaitingReason = "ContainerCreating";
    tunnel.fail("lost connection to pod");
    await vi.waitFor(() => expect(output()).toContain("Reconnecting on the same port"));
    await vi.advanceTimersByTimeAsync(1000);
    signal(name);
    await running;
    expect(calls().filter(({ args }) => args.includes("port-forward"))).toHaveLength(1);
    expect(process.exitCode).toBe(0);
    expectCleaned();
  });

  it.each(["CrashLoopBackOff", "ContainerCreating"])("bounds replacement-pod readiness for %s", async reason => {
    const running = run("--web", "--port", "19119");
    await vi.waitFor(() => expect(output()).toContain("Port-forward active"));
    podPhase = "Pending";
    podWaitingReason = reason;
    tunnel.fail("lost connection to pod");
    await vi.waitFor(() => expect(output()).toContain("Reconnecting on the same port"));
    await vi.advanceTimersByTimeAsync(181_000);
    await running;
    expect(errors()).toContain(reason === "ContainerCreating" ? "timed out after 3m" : reason);
    expect(calls().filter(({ args }) => args.includes("port-forward"))).toHaveLength(1);
    expect(process.exitCode).toBe(1);
    expectCleaned();
  });

  it.each(["Forbidden", "Unauthorized"])("does not retry a replacement-pod readiness %s denial", async denial => {
    const running = run("--web", "--port", "19119");
    await vi.waitFor(() => expect(output()).toContain("Port-forward active"));
    execute.mockRejectedValueOnce(new Error(denial));
    tunnel.fail("lost connection to pod");
    await vi.waitFor(() => expect(output()).toContain("Reconnecting on the same port"));
    await vi.advanceTimersByTimeAsync(1000);
    await running;
    expect(errors()).toContain(denial);
    expect(calls().filter(({ args }) => args.includes("port-forward"))).toHaveLength(1);
    expect(process.exitCode).toBe(1);
    expectCleaned();
  });

  it.each(["SIGINT", "SIGTERM"] as const)("cancels reconnect backoff on %s", async name => {
    const running = run("--web", "--port", "19119");
    await vi.waitFor(() => expect(output()).toContain("Port-forward active"));
    tunnel.fail("lost connection to pod");
    await vi.waitFor(() => expect(output()).toContain("Reconnecting on the same port"));
    signal(name);
    await running;
    await vi.advanceTimersByTimeAsync(1000);
    expect(calls().filter(({ args }) => args.includes("port-forward"))).toHaveLength(1);
    expect(process.exitCode).toBe(0);
    expectCleaned();
  });

  it("does not retry authorization failures after readiness", async () => {
    const running = run("--web", "--port", "19119");
    await vi.waitFor(() => expect(output()).toContain("Port-forward active"));
    tunnel.fail("lost connection to pod: Forbidden");
    await running;
    expect(errors()).toContain("Forbidden");
    expect(calls().filter(({ args }) => args.includes("port-forward"))).toHaveLength(1);
    expect(process.exitCode).toBe(1);
    expectCleaned();
  });

  it("reports a reconnect port collision without borrowing another listener", async () => {
    const running = run("--web", "--port", "19119");
    await vi.waitFor(() => expect(output()).toContain("Port-forward active"));
    const first = tunnel;
    tunnel = forward();
    onForward = () => tunnel.fail("address already in use");
    first.fail("lost connection to pod");
    await vi.waitFor(() => expect(output()).toContain("Reconnecting on the same port"));
    await vi.advanceTimersByTimeAsync(1000);
    await running;
    expect(errors()).toContain("address already in use");
    expect(calls().filter(({ args }) => args.includes("port-forward"))).toHaveLength(2);
    expect(createServer).not.toHaveBeenCalled();
    expect(process.exitCode).toBe(1);
    expectCleaned();
  });

  it("reports an unavailable dashboard and does not open a browser", async () => {
    fetchMock.mockResolvedValue({ ok: false, status: 503 });
    await run("--web", "--port", "19119");
    expect(errors()).toContain("HTTP 503");
    expect(errors()).toContain("HERMES_DASHBOARD_ENABLED");
    expect(calls().every(({ file }) => file === "kubectl")).toBe(true);
    expect(output()).not.toContain("Port-forward active");
    expect(process.exitCode).toBe(1);
    expectCleaned();
  });

  it("keeps the usable URL and tunnel when no browser opener is installed", async () => {
    const original = execute.getMockImplementation()!;
    execute.mockImplementation((file, args) => file !== "kubectl" ? Promise.reject(new Error("opener missing")) : original(file, args));
    const running = run("--web", "--port", "19119");
    await vi.waitFor(() => expect(output()).toContain("Could not open a browser automatically"));
    expect(output()).toContain("http://127.0.0.1:19119/");
    tunnel.finish();
    await running;
    expect(process.exitCode).toBe(0);
  });

  it("resets Hermes with the selected context before forwarding, without token access", async () => {
    const running = run("--web", "--port", "19119", "--reset");
    await vi.waitFor(() => expect(output()).toContain("Port-forward active"));
    const invocations = calls();
    const resetIndex = invocations.findIndex(({ args }) => args.includes("restart"));
    expect(invocations[resetIndex].args).toEqual(["--context", "kind-beta", "rollout", "restart", "-n", "kars-sre", "deploy/sre"]);
    expect(resetIndex).toBeLessThan(invocations.findIndex(({ args }) => args.includes("port-forward")));
    expectNoExecOrSecret();
    tunnel.finish();
    await running;
  });

  it.each(["Kata", "unready"])("does not silently substitute a shell for a %s web failure", async condition => {
    if (condition === "Kata") runtimeClass = "kata-mshv-vm-isolation";
    else podPhase = "Pending";
    await run("--web", "--port", "19119");
    expect(calls().some(({ args }) => args.includes("port-forward"))).toBe(false);
    expectNoExecOrSecret();
    expect(process.exitCode).toBe(1);
    expect(output() + errors()).toContain(condition === "Kata" ? "Kata" : "CrashLoopBackOff");
  });
});

describe("existing connection routing", () => {
  it.each([{ args: [] }, { args: ["--shell"] }])("keeps Hermes chat for $args", async ({ args }) => {
    await run(...args);
    expect(calls().find(({ args }) => args.includes("exec"))?.args).toEqual([
      "--context", "kind-beta", "exec", "-it", "-n", "kars-sre", "deploy/sre", "-c", "agent", "--",
      "env", "HOME=/sandbox", "HERMES_HOME=/sandbox/.hermes", "hermes", "chat", "--accept-hooks",
    ]);
    expect(calls().some(({ args }) => args.includes("port-forward") || args.includes("gateway-token"))).toBe(false);
  });

  it.each([{ args: [] }, { args: ["--web"] }])("keeps OpenClaw gateway token and deployment forwarding for $args", async ({ args }) => {
    runtime = "OpenClaw";
    const running = run(...args, "--port", "19119");
    await vi.waitFor(() => expect(output()).toContain("Port-forward active"));
    expect(calls().some(({ args }) => args.includes("gateway-token"))).toBe(true);
    expect(calls().find(({ args }) => args.includes("port-forward"))?.args).toEqual([
      "--context", "kind-beta", "port-forward", "--address", "127.0.0.1", "-n", "kars-sre", "deploy/sre", "19119:18789",
    ]);
    expect(output()).toContain("http://localhost:19119/#token=test-gateway-token");
    expect(calls().some(({ args }) => args.includes("exec"))).toBe(false);
    tunnel.finish();
    await running;
    expectCleaned();
  });

  it("keeps OpenClaw's missing-token refusal", async () => {
    runtime = "OpenClaw";
    secret = "";
    await run("--web");
    expect(output()).toContain("Gateway token not found");
    expect(calls().some(({ args }) => args.includes("port-forward"))).toBe(false);
  });

  it("keeps OpenClaw bash routing and carries the explicit context", async () => {
    runtime = "OpenClaw";
    await run("--shell");
    expect(calls().find(({ args }) => args.includes("exec"))?.args).toEqual([
      "--context", "kind-beta", "exec", "-it", "-n", "kars-sre", "deploy/sre", "-c", "openclaw", "--", "/bin/bash", "--login",
    ]);
    expect(calls().some(({ args }) => args.includes("gateway-token") || args.includes("port-forward"))).toBe(false);
  });
});
