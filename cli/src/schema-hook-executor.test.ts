// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";
import type { SchemaExecute } from "./lib/schema-documents.js";

const mocked = vi.hoisted(() => ({ execFile: vi.fn(), readFile: vi.fn(), mkdtemp: vi.fn(),
  writeFile: vi.fn(), rm: vi.fn(), prepare: vi.fn() }));
vi.mock("node:child_process", () => ({ execFile: mocked.execFile }));
vi.mock("node:fs/promises", () => ({ readFile: mocked.readFile, mkdtemp: mocked.mkdtemp,
  writeFile: mocked.writeFile, rm: mocked.rm }));
vi.mock("./lib/core-helm-schema-hook.js", () => ({ preparePendingCoreHelmSchemas: mocked.prepare }));
import { runSchemaHook } from "./schema-hook.js";

const args = ["--release", "kars", "--namespace", "kars-system", "--revision", "1",
  "--operation", "install", "--installer-helm-version", "v4.1.3"];
const account = "/var/run/secrets/kubernetes.io/serviceaccount";
const success = { schemas: 1, published: true, revision: 1, manifestDigest: "test-digest" };

beforeEach(() => {
  vi.resetAllMocks();
  vi.stubEnv("KUBERNETES_SERVICE_HOST", "10.0.0.1");
  vi.stubEnv("KUBERNETES_SERVICE_PORT", "443");
  vi.stubEnv("PATH", "/schema-tools");
  vi.stubEnv("KUBECONFIG", "/untrusted/config");
  vi.stubEnv("HELM_KUBECONTEXT", "untrusted-context");
  vi.stubEnv("PRIVATE_TEST_CREDENTIAL", "must-not-be-inherited");
  mocked.readFile.mockImplementation(async (path: string) => {
    const value = { [`${account}/namespace`]: "kars-system\n", [`${account}/token`]: "test-token",
      [`${account}/ca.crt`]: "test-ca" }[path];
    if (!value) throw new Error("Unexpected credential read");
    return value;
  });
  mocked.mkdtemp.mockResolvedValue("/tmp/kars-schema-test");
  mocked.writeFile.mockResolvedValue(undefined);
  mocked.rm.mockResolvedValue(undefined);
  mocked.execFile.mockImplementation((_file: string, _args: string[], _options: unknown,
    callback: (error: Error | null, stdout: string, stderr: string) => void) => {
    queueMicrotask(() => callback(null, "response", ""));
    return { stdin: { on: vi.fn(), end: vi.fn() }, kill: vi.fn() };
  });
  mocked.prepare.mockImplementation(async (execute: SchemaExecute) => {
    await execute("helm", ["history", "kars", "-n", "kars-system"], { stdio: "pipe", timeout: 800 });
    await execute("kubectl", ["get", "namespaces", "kars-system"], { stdio: "pipe", timeout: 500 });
    return success;
  });
});
afterEach(() => vi.unstubAllEnvs());

describe("in-cluster schema executor", () => {
  it("pins both clients to projected credentials without copying the bearer token", async () => {
    expect(await runSchemaHook(args)).toEqual(success);
    expect(mocked.execFile).toHaveBeenCalledTimes(2);
    const [path, text, options] = mocked.writeFile.mock.calls[0];
    expect(path).toBe("/tmp/kars-schema-test/config.json");
    expect(options).toEqual({ mode: 0o600, flag: "wx" });
    expect(text).not.toContain("test-token");
    const config = JSON.parse(text);
    expect(config.clusters).toEqual([{ name: "in-cluster", cluster: { server: "https://10.0.0.1:443",
      "certificate-authority": `${account}/ca.crt` } }]);
    expect(config.users).toEqual([{ name: "schema-hook", user: { tokenFile: `${account}/token` } }]);
    expect(config.contexts[0].context.namespace).toBe("kars-system");
    for (const call of mocked.execFile.mock.calls) {
      expect(call[1].slice(0, 2)).toEqual(["--kubeconfig", path]);
      expect(call[2].env).toEqual({ PATH: "/schema-tools", HOME: "/tmp", XDG_CACHE_HOME: "/tmp/cache", HELM_DRIVER: "secret" });
      expect(call[2].maxBuffer).toBe(8 * 1024 * 1024);
      expect(call[2].killSignal).toBe("SIGKILL");
    }
    expect(mocked.execFile.mock.calls.map(call => call[2].timeout)).toEqual([800, 500]);
    expect(mocked.rm).toHaveBeenCalledExactlyOnceWith("/tmp/kars-schema-test", { recursive: true, force: true });
  });

  it.each([
    { host: "api.example.test", port: "443" }, { host: "::1", port: "443" },
    { host: "", port: "443" }, { host: "10.0.0.1", port: "0" },
    { host: "10.0.0.1", port: "65536" }, { host: "10.0.0.1", port: "443/evil" },
  ])("refuses an unsupported endpoint $host:$port before reading credentials", async ({ host, port }) => {
    vi.stubEnv("KUBERNETES_SERVICE_HOST", host);
    vi.stubEnv("KUBERNETES_SERVICE_PORT", port);
    await expect(runSchemaHook(args)).rejects.toThrow("reviewed in-cluster IPv4");
    expect(mocked.readFile).not.toHaveBeenCalled();
    expect(mocked.execFile).not.toHaveBeenCalled();
  });

  it.each(["namespace", "token", "ca.crt"])("refuses a missing projected %s without local fallback", async part => {
    const read = mocked.readFile.getMockImplementation()!;
    mocked.readFile.mockImplementation(async (path: string) => {
      if (path === `${account}/${part}`) throw new Error("unavailable");
      return read(path);
    });
    await expect(runSchemaHook(args)).rejects.toThrow("unavailable");
    expect(mocked.execFile).not.toHaveBeenCalled();
    expect(mocked.mkdtemp).not.toHaveBeenCalled();
  });

  it.each([
    { part: "namespace", value: "another-namespace" },
    { part: "token", value: " \n" }, { part: "ca.crt", value: "" },
  ])("refuses mismatched or empty projected $part", async ({ part, value }) => {
    const read = mocked.readFile.getMockImplementation()!;
    mocked.readFile.mockImplementation(async (path: string) => path === `${account}/${part}` ? value : read(path));
    await expect(runSchemaHook(args)).rejects.toThrow("own projected service-account");
    expect(mocked.mkdtemp).not.toHaveBeenCalled();
    expect(mocked.execFile).not.toHaveBeenCalled();
  });

  it("removes only its generated directory when kubeconfig creation fails", async () => {
    mocked.writeFile.mockRejectedValue(new Error("write failed"));
    await expect(runSchemaHook(args)).rejects.toThrow("write failed");
    expect(mocked.prepare).not.toHaveBeenCalled();
    expect(mocked.execFile).not.toHaveBeenCalled();
    expect(mocked.rm).toHaveBeenCalledExactlyOnceWith("/tmp/kars-schema-test", { recursive: true, force: true });
  });

  it("cleans up the generated config after staging refuses without retrying", async () => {
    mocked.prepare.mockRejectedValue(new Error("staging refused"));
    await expect(runSchemaHook(args)).rejects.toThrow("staging refused");
    expect(mocked.prepare).toHaveBeenCalledTimes(1);
    expect(mocked.rm).toHaveBeenCalledTimes(1);
  });

  it("rejects an executable outside the two trusted clients", async () => {
    mocked.prepare.mockImplementation((execute: SchemaExecute) => execute("sh", [], { stdio: "pipe", timeout: 800 }));
    await expect(runSchemaHook(args)).rejects.toThrow("only executes Helm and kubectl");
    expect(mocked.execFile).not.toHaveBeenCalled();
    expect(mocked.rm).toHaveBeenCalledTimes(1);
  });
});
