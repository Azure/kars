// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { chmodSync, existsSync, lstatSync, mkdtempSync, readFileSync, readdirSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import path from "node:path";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import type { Execute } from "../../lib/private-activation.js";
import type { BetaExecute, BetaTools } from "./kind-beta.js";
import { createKindBetaInstallerWriter, withKindBetaInstallerWriter } from "./kind-beta-installer-writer.js";

const name = "kars-provider-installer", namespace = "kars-system", storeName = "kars-inference-providers";
const subject = `system:serviceaccount:${namespace}:${name}`;
const tools: BetaTools = { kubectl: "controlled-kubectl", kind: "kind", helm: "helm", runtime: "docker", env: { PATH: process.env.PATH } };
const patchArgs = ["patch", "secret", storeName, "-n", namespace, "--type=json", "--patch-file=/dev/stdin"];
const conditions = ["Ready", "WriterReady", "IntegrationReady", "PrivateConsumptionReady"].map(type => ({ type, status: "True" }));
let root: string, ca: string;
const directories: string[] = [];

beforeAll(() => {
  root = mkdtempSync(path.join(process.cwd(), ".installer-writer-cert-"));
  execFileSync("openssl", ["req", "-new", "-x509", "-newkey", "rsa:2048", "-nodes", "-sha256", "-days", "1",
    "-subj", "/CN=Controlled Kind CA", "-addext", "basicConstraints=critical,CA:TRUE",
    "-keyout", path.join(root, "ca.key"), "-out", path.join(root, "ca.crt")], { stdio: "pipe" });
  ca = readFileSync(path.join(root, "ca.crt")).toString("base64");
  execFileSync("openssl", ["req", "-new", "-x509", "-key", path.join(root, "ca.key"), "-sha256", "-days", "1",
    "-subj", "/CN=Not a CA", "-addext", "basicConstraints=critical,CA:FALSE", "-out", path.join(root, "leaf.crt")], { stdio: "pipe" });
});
afterEach(() => { vi.useRealTimers(); for (const directory of directories.splice(0)) rmSync(directory, { recursive: true, force: true }); });
afterAll(() => { if (root) rmSync(root, { recursive: true, force: true }); });

function fixture() {
  const directory = mkdtempSync(path.join(process.cwd(), ".installer-writer-test-"));
  directories.push(directory);
  const writer = { name, uid: "sa-uid" };
  const account: any = { apiVersion: "v1", kind: "ServiceAccount", metadata: { name, namespace, uid: writer.uid, resourceVersion: "1" },
    automountServiceAccountToken: false };
  const grant: any = { metadata: { name: "workspace", namespace, uid: "grant-uid", resourceVersion: "2", generation: 3 },
    spec: { enabled: true, workspaceUid: "namespace-uid", writers: [{ namespace, ...writer }],
      integrationStores: [{ purpose: "providers", secret: { name: storeName, uid: "store-uid" } }] },
    status: { phase: "Ready", observedGeneration: 3, conditions: structuredClone(conditions) } };
  const store: any = { type: "Opaque", metadata: { name: storeName, namespace, uid: "store-uid", resourceVersion: "4",
    annotations: { "kars.azure.com/credential-store-grant-uid": "grant-uid" } } };
  const workspace: any = { metadata: { name: namespace, uid: "namespace-uid", resourceVersion: "5" } };
  const iat = Math.floor(Date.now() / 1000);
  const claims: any = { iat, exp: iat + 600, sub: subject, "kubernetes.io": { namespace,
    serviceaccount: { name, uid: writer.uid }, secret: { name: storeName, uid: store.metadata.uid } } };
  const encode = (value: unknown) => Buffer.from(JSON.stringify(value)).toString("base64url");
  const token = () => `${encode({ alg: "RS256", typ: "JWT" })}.${encode(claims)}.controlled_signature`;
  const response: any = { apiVersion: "authentication.k8s.io/v1", kind: "TokenRequest",
    status: { token: token(), expirationTimestamp: new Date(claims.exp * 1000).toISOString() } };
  const settings = { server: "https://127.0.0.1:6443", ca,
    whoami: { status: { userInfo: { username: subject, uid: writer.uid } } } as any };
  const configs: any[] = [], configPaths: string[] = [];
  const execute = vi.fn<Execute>(async (args, input) => {
    if (args[0] === "wait") return "";
    if (args[0] === "get") {
      const object = args[1] === "serviceaccount" ? account : args[1] === "secret" ? store : args[1] === "namespace" ? workspace : grant;
      return JSON.stringify(object);
    }
    if (args[0] === "create") {
      if (args[1] === "--raw") {
        expect(JSON.parse(input!)).toEqual({ apiVersion: "authentication.k8s.io/v1", kind: "TokenRequest", spec: {
          audiences: [], expirationSeconds: 600, boundObjectRef: { apiVersion: "v1", kind: "Secret", name: storeName, uid: "store-uid" },
        } });
        expect(args).toEqual(["create", "--raw", `/api/v1/namespaces/${namespace}/serviceaccounts/${name}/token`, "-f", "-"]);
        return JSON.stringify(response);
      }
      return JSON.stringify(account);
    }
    if (args[0] === "config") {
      expect(args.slice(0, 6)).toEqual(["config", "view", "--minify", "--raw", "-o", args[5]]);
      if (args[5] === "jsonpath={.clusters[0].cluster.server}") return settings.server;
      if (args[5] === "jsonpath={.clusters[0].cluster.certificate-authority-data}") return settings.ca;
    }
    throw new Error("Unexpected operator command");
  });
  const local = vi.fn<BetaExecute>(async (file, args, options) => {
    expect(file).toBe(tools.kubectl);
    expect(args[0]).toBe("--kubeconfig"); expect(args[2]).toBe("--context");
    const configPath = args[1], config = JSON.parse(readFileSync(configPath, "utf8"));
    configs.push(config); configPaths.push(configPath);
    expect(lstatSync(configPath).mode & 0o777).toBe(0o600);
    expect(lstatSync(path.dirname(configPath)).mode & 0o777).toBe(0o700);
    expect(lstatSync(configPath).uid).toBe(process.getuid!());
    expect(config["current-context"]).toBe(args[3]);
    expect(JSON.stringify(args)).not.toContain(response.status.token);
    expect(JSON.stringify(options?.env)).not.toContain(response.status.token);
    if (args[4] === "auth") return { stdout: JSON.stringify(settings.whoami) };
    expect(args.slice(4)).toEqual(patchArgs);
    store.data = JSON.parse(options!.input!)[2].value;
    return { stdout: "provider patch result" };
  });
  const patch = () => JSON.stringify([
    { op: "test", path: "/metadata/uid", value: "store-uid" },
    { op: "test", path: "/metadata/resourceVersion", value: "4" },
    { op: "add", path: "/data", value: { KARS_PROVIDER_OPENAI_API_KEY: Buffer.from("private-provider-input").toString("base64") } },
  ]);
  const run = <T>(work: (write: Execute) => Promise<T>) => withKindBetaInstallerWriter(execute, writer, directory, tools, local, work);
  const cleaned = () => {
    expect(readdirSync(directory)).toEqual([]);
    for (const filename of configPaths) expect(existsSync(filename)).toBe(false);
  };
  return { directory, writer, account, grant, store, workspace, claims, token, response, settings, execute, local, configs, configPaths, patch, run, cleaned };
}

describe("dedicated Kind provider installer account", () => {
  it("creates only a non-automounted ServiceAccount and returns its live UID", async () => {
    const f = fixture();
    await expect(createKindBetaInstallerWriter(f.execute)).resolves.toEqual(f.writer);
    expect(f.execute).toHaveBeenCalledExactlyOnceWith(["create", "-f", "-", "-o", "json"], JSON.stringify({
      apiVersion: "v1", kind: "ServiceAccount", metadata: { name, namespace }, automountServiceAccountToken: false,
    }));
  });
  it("never adopts an existing account or retries a failed create", async () => {
    const execute = vi.fn<Execute>(async () => { throw new Error("AlreadyExists"); });
    await expect(createKindBetaInstallerWriter(execute)).rejects.toThrow("AlreadyExists");
    expect(execute).toHaveBeenCalledTimes(1);
  });
  it.each([
    ["missing UID", (a: any) => { delete a.metadata.uid; }],
    ["blank UID", (a: any) => { a.metadata.uid = " "; }],
    ["wrong name", (a: any) => { a.metadata.name = "default"; }],
    ["wrong namespace", (a: any) => { a.metadata.namespace = "other"; }],
    ["automount enabled", (a: any) => { a.automountServiceAccountToken = true; }],
    ["automount implicit", (a: any) => { delete a.automountServiceAccountToken; }],
    ["terminating", (a: any) => { a.metadata.deletionTimestamp = "now"; }],
    ["static token", (a: any) => { a.secrets = [{ name: "old-token" }]; }],
  ] as const)("rejects create response with %s", async (_label, mutate) => {
    const f = fixture(); mutate(f.account);
    await expect(createKindBetaInstallerWriter(f.execute)).rejects.toThrow();
  });
});

describe("short-lived Kind provider installer writer", () => {
  it("uses real TokenRequest input, private files, TLS-only public configuration and one stdin CAS patch", async () => {
    const f = fixture();
    await expect(f.run(async write => {
      expect(f.local).toHaveBeenCalledTimes(1);
      expect(f.execute.mock.calls.some(([args]) => args.includes("--for=condition=PrivateConsumptionReady"))).toBe(true);
      return write(patchArgs, f.patch());
    })).resolves.toBe("provider patch result");
    expect(f.local).toHaveBeenCalledTimes(2);
    expect(f.local.mock.calls[0][1].slice(4)).toEqual(["auth", "whoami", "-o", "json"]);
    const config = f.configs[0], context = config["current-context"];
    expect(config).toEqual({ apiVersion: "v1", kind: "Config", clusters: [{ name: context, cluster: {
      server: f.settings.server, "certificate-authority-data": ca,
    } }], users: [{ name: context, user: { token: f.response.status.token } }],
    contexts: [{ name: context, context: { cluster: context, user: context, namespace } }], "current-context": context });
    expect(f.configs[1]).toEqual(config);
    expect(f.execute.mock.calls.some(([args]) => args[0] === "patch")).toBe(false);
    expect(JSON.stringify(f.execute.mock.calls.map(([args]) => args))).not.toContain(f.response.status.token);
    f.cleaned();
  });
  it.each(["https://localhost:12345", "https://[::1]:12345/"])("accepts the explicit Kind loopback endpoint %s", async server => {
    const f = fixture(); f.settings.server = server;
    const value = { completed: true };
    await expect(f.run(async () => value)).resolves.toBe(value); f.cleaned();
  });
  it("propagates work failures and cleans up without touching caller-owned files", async () => {
    const f = fixture(), failure = new Error("work failed");
    await expect(f.run(async () => { throw failure; })).rejects.toBe(failure);
    f.cleaned(); expect(existsSync(f.directory)).toBe(true);
  });
  it("never overwrites caller files and uses fresh isolated contexts on separate invocations", async () => {
    const f = fixture(), sentinel = path.join(f.directory, "kubeconfig.json");
    writeFileSync(sentinel, "existing operator config", { mode: 0o600, flag: "wx" });
    await f.run(async () => {}); await f.run(async () => {});
    expect(f.configs[0]["current-context"]).not.toBe(f.configs[1]["current-context"]);
    expect(f.configPaths[0]).not.toBe(f.configPaths[1]);
    expect(readFileSync(sentinel, "utf8")).toBe("existing operator config");
    expect(readdirSync(f.directory)).toEqual(["kubeconfig.json"]);
    for (const file of f.configPaths) expect(existsSync(file)).toBe(false);
  });
  it("does not send a write that was left pending when work returned", async () => {
    const f = fixture(), execute = f.execute.getMockImplementation()!;
    let pending!: Promise<string>, release!: () => void;
    const blocked = new Promise<void>(resolve => { release = resolve; });
    await f.run(async write => {
      f.execute.mockImplementation(async (...args) => { await blocked; return execute(...args); });
      pending = write(patchArgs, f.patch());
    });
    release();
    await expect(pending).rejects.toThrow("closed");
    expect(f.local).toHaveBeenCalledTimes(1); f.cleaned();
  });
  it("closes the captured writer when work returns", async () => {
    const f = fixture(); let saved!: Execute;
    await f.run(async write => { saved = write; });
    await expect(saved(patchArgs, f.patch())).rejects.toThrow("closed");
    expect(f.local).toHaveBeenCalledTimes(1); f.cleaned();
  });
  it("does not reuse the token for a second patch", async () => {
    const f = fixture();
    await f.run(async write => {
      await write(patchArgs, f.patch());
      await expect(write(patchArgs, f.patch())).rejects.toThrow("already used");
    });
    expect(f.local).toHaveBeenCalledTimes(2); f.cleaned();
  });
  it.each([
    ["subject", { username: "system:admin", uid: "sa-uid" }],
    ["UID", { username: subject, uid: "replaced" }],
    ["missing UID", { username: subject }],
  ] as const)("refuses authenticated %s mismatch before work", async (_label, userInfo) => {
    const f = fixture(), work = vi.fn(); f.settings.whoami = { status: { userInfo } };
    await expect(f.run(work)).rejects.toThrow(/subject|UID/);
    expect(work).not.toHaveBeenCalled(); f.cleaned();
  });
  it.each([
    ["replaced SA", (f: ReturnType<typeof fixture>) => { f.account.metadata.uid = "replaced"; }],
    ["wrong SA namespace", (f: ReturnType<typeof fixture>) => { f.account.metadata.namespace = "other"; }],
    ["terminating SA", (f: ReturnType<typeof fixture>) => { f.account.metadata.deletionTimestamp = "now"; }],
    ["automount enabled", (f: ReturnType<typeof fixture>) => { f.account.automountServiceAccountToken = true; }],
    ["replaced store", (f: ReturnType<typeof fixture>) => { f.store.metadata.uid = "replaced"; }],
    ["terminating store", (f: ReturnType<typeof fixture>) => { f.store.metadata.deletionTimestamp = "now"; }],
    ["wrong store type", (f: ReturnType<typeof fixture>) => { f.store.type = "kubernetes.io/service-account-token"; }],
    ["wrong store namespace", (f: ReturnType<typeof fixture>) => { f.store.metadata.namespace = "other"; }],
    ["foreign store", (f: ReturnType<typeof fixture>) => { f.store.metadata.annotations["kars.azure.com/credential-store-grant-uid"] = "foreign"; }],
    ["populated store", (f: ReturnType<typeof fixture>) => { f.store.data = { retained: "c2VjcmV0" }; }],
    ["replaced grant", (f: ReturnType<typeof fixture>) => { f.grant.metadata.uid = "replaced"; }],
    ["terminating grant", (f: ReturnType<typeof fixture>) => { f.grant.metadata.deletionTimestamp = "now"; }],
    ["disabled grant", (f: ReturnType<typeof fixture>) => { f.grant.spec.enabled = false; }],
    ["unenrolled writer", (f: ReturnType<typeof fixture>) => { f.grant.spec.writers = []; }],
    ["wrong enrolled UID", (f: ReturnType<typeof fixture>) => { f.grant.spec.writers[0].uid = "replaced"; }],
    ["duplicate writer", (f: ReturnType<typeof fixture>) => { f.grant.spec.writers.push(f.grant.spec.writers[0]); }],
    ["wrong store purpose", (f: ReturnType<typeof fixture>) => { f.grant.spec.integrationStores[0].purpose = "github-app"; }],
    ["duplicate store", (f: ReturnType<typeof fixture>) => { f.grant.spec.integrationStores.push(f.grant.spec.integrationStores[0]); }],
    ["missing store", (f: ReturnType<typeof fixture>) => { f.grant.spec.integrationStores = []; }],
    ["stale generation", (f: ReturnType<typeof fixture>) => { f.grant.status.observedGeneration--; }],
    ["integration error", (f: ReturnType<typeof fixture>) => { f.grant.status.integrationError = "failed"; }],
    ["unready grant", (f: ReturnType<typeof fixture>) => { f.grant.status.conditions.pop(); }],
    ["replaced namespace", (f: ReturnType<typeof fixture>) => { f.workspace.metadata.uid = "replaced"; }],
  ] as const)("preserves credentials and never requests a token for %s", async (_label, mutate) => {
    const f = fixture(); mutate(f); const before = structuredClone(f.store), work = vi.fn();
    await expect(f.run(work)).rejects.toThrow();
    expect(work).not.toHaveBeenCalled(); expect(f.local).not.toHaveBeenCalled();
    expect(f.execute.mock.calls.some(([args]) => args[1] === "--raw")).toBe(false);
    expect(f.store).toEqual(before); f.cleaned();
  });
  it("rejects a grant replacement while waiting even when the replacement is Ready", async () => {
    const f = fixture(), execute = f.execute.getMockImplementation()!;
    f.execute.mockImplementation(async (args, input) => {
      if (args[0] === "wait") {
        f.grant.metadata.uid = "replacement";
        f.store.metadata.annotations["kars.azure.com/credential-store-grant-uid"] = "replacement";
      }
      return execute(args, input);
    });
    await expect(f.run(vi.fn())).rejects.toThrow(/changed during readiness verification/);
    expect(f.local).not.toHaveBeenCalled(); f.cleaned();
  });
  it.each(["sa", "store-version", "grant", "namespace", "populated"])("revalidates %s after whoami and before invoking work", async changed => {
    const f = fixture(), local = f.local.getMockImplementation()!, work = vi.fn();
    f.local.mockImplementation(async (...args) => {
      const result = await local(...args);
      if (changed === "sa") f.account.metadata.uid = "new";
      if (changed === "store-version") f.store.metadata.resourceVersion = "new";
      if (changed === "grant") { f.grant.metadata.uid = "new"; f.store.metadata.annotations["kars.azure.com/credential-store-grant-uid"] = "new"; }
      if (changed === "namespace") { f.workspace.metadata.uid = "new"; f.grant.spec.workspaceUid = "new"; }
      if (changed === "populated") f.store.data = { retained: "c2VjcmV0" };
      return result;
    });
    await expect(f.run(work)).rejects.toThrow(/changed/); expect(work).not.toHaveBeenCalled(); f.cleaned();
  });
  it("rejects a replaced store even if the grant is concurrently re-enrolled during work", async () => {
    const f = fixture();
    await expect(f.run(async write => {
      f.store.metadata.uid = "replacement"; f.grant.spec.integrationStores[0].secret.uid = "replacement";
      return write(patchArgs, f.patch());
    })).rejects.toThrow("authority changed");
    expect(f.local).toHaveBeenCalledTimes(1); f.cleaned();
  });
  it.each([
    "http://127.0.0.1:6443", "https://example.com", "https://127.0.0.2:6443", "https://127.1:6443", "https://2130706433:6443",
    "https://user:pass@localhost:6443", "https://localhost:6443?secret", "https://localhost:6443#fragment", "https://localhost:6443?",
    "https://localhost:6443/other", "https://localhost.evil:6443", " https://localhost:6443", "https://[::ffff:127.0.0.1]:6443",
  ])("rejects endpoint %s without issuing credentials", async endpoint => {
    const f = fixture(); f.settings.server = endpoint;
    await expect(f.run(vi.fn())).rejects.toThrow("HTTPS loopback");
    expect(f.execute.mock.calls.some(([args]) => args[1] === "--raw")).toBe(false); f.cleaned();
  });
  it.each(["", "invalid-base64", Buffer.from("not a certificate").toString("base64")])("rejects invalid X509 CA %s", async certificate => {
    const f = fixture(); f.settings.ca = certificate;
    await expect(f.run(vi.fn())).rejects.toThrow("X509 CA"); expect(f.local).not.toHaveBeenCalled(); f.cleaned();
  });
  it.each(["expired", "not-yet-valid", "leaf", "multiple-certificates"])("rejects %s public trust", async kind => {
    const f = fixture();
    if (kind === "expired" || kind === "not-yet-valid") {
      vi.useFakeTimers(); vi.setSystemTime(Date.now() + (kind === "expired" ? 2 : -2) * 86_400_000);
    }
    if (kind === "leaf") f.settings.ca = readFileSync(path.join(root, "leaf.crt")).toString("base64");
    if (kind === "multiple-certificates") f.settings.ca = Buffer.concat([Buffer.from(ca, "base64"), Buffer.from(ca, "base64")]).toString("base64");
    await expect(f.run(vi.fn())).rejects.toThrow("X509 CA"); expect(f.local).not.toHaveBeenCalled(); f.cleaned();
  });
  it.each(["expired", "unbounded", "malformed", "wrong-subject", "wrong-uid", "wrong-secret", "missing-token", "wrong-kind", "mismatched-expiry"])("rejects %s TokenRequest response without writing a kubeconfig", async kind => {
    const f = fixture();
    if (kind === "expired") f.response.status.expirationTimestamp = new Date(Date.now() - 1000).toISOString();
    if (kind === "unbounded") f.response.status.expirationTimestamp = new Date(Date.now() + 3_600_000).toISOString();
    if (kind === "malformed") f.response.status.token = "secret-token-that-is-not-a-jwt";
    if (kind === "wrong-subject") { f.claims.sub = "system:admin"; f.response.status.token = f.token(); }
    if (kind === "wrong-uid") { f.claims["kubernetes.io"].serviceaccount.uid = "replacement"; f.response.status.token = f.token(); }
    if (kind === "wrong-secret") { f.claims["kubernetes.io"].secret.uid = "replacement"; f.response.status.token = f.token(); }
    if (kind === "missing-token") delete f.response.status.token;
    if (kind === "wrong-kind") f.response.kind = "Secret";
    if (kind === "mismatched-expiry") f.response.status.expirationTimestamp = new Date(Date.now() + 100_000).toISOString();
    await expect(f.run(vi.fn())).rejects.toThrow("TokenRequest response");
    expect(f.local).not.toHaveBeenCalled(); f.cleaned();
  });
  it("refuses expired authority immediately before the patch", async () => {
    const f = fixture();
    await expect(f.run(async write => {
      vi.useFakeTimers(); vi.setSystemTime(Date.now() + 601_000);
      return write(patchArgs, f.patch());
    })).rejects.toThrow("token expired");
    expect(f.local).toHaveBeenCalledTimes(1); f.cleaned();
  });
  it.each(["token-request", "whoami", "patch"])("sanitizes token-bearing %s failures and cleans up", async stage => {
    const f = fixture(), execute = f.execute.getMockImplementation()!, local = f.local.getMockImplementation()!;
    f.execute.mockImplementation(async (...args) => {
      if (stage === "token-request" && args[0][1] === "--raw") throw new Error(`stderr ${f.response.status.token}`);
      return execute(...args);
    });
    f.local.mockImplementation(async (...args) => {
      if (stage === "whoami" || stage === "patch" && args[1][4] === "patch") throw new Error(`stderr ${f.response.status.token}`);
      return local(...args);
    });
    const failure = await f.run(write => write(patchArgs, f.patch())).catch(error => error);
    expect(failure).toBeInstanceOf(Error); expect(failure.message).toContain("private command output is not displayed");
    expect(String(failure)).not.toContain(f.response.status.token); expect(failure.cause).toBeUndefined(); f.cleaned();
  });
  it.each(["get", "wrong-secret", "other-namespace", "extra-flags", "inline-patch", "missing-input", "malformed-json",
    "missing-cas", "wrong-uid", "wrong-version", "metadata-change", "injected-settings", "bad-base64", "empty-data", "extra-operation"])("rejects %s instead of exposing a generic kubectl executor", async operation => {
    const f = fixture(), args = [...patchArgs]; let input: string | undefined = f.patch(); const patch = JSON.parse(input);
    if (operation === "get") args[0] = "get";
    if (operation === "wrong-secret") args[2] = "other-secret";
    if (operation === "other-namespace") args[4] = "other";
    if (operation === "extra-flags") args.push("--as=system:admin");
    if (operation === "inline-patch") args[6] = `--patch=${input}`;
    if (operation === "missing-cas") patch.shift();
    if (operation === "wrong-uid") patch[0].value = "other";
    if (operation === "wrong-version") patch[1].value = "other";
    if (operation === "metadata-change") patch[2].path = "/metadata";
    if (operation === "injected-settings") patch[2].value = { HTTPS_PROXY: "eA==" };
    if (operation === "bad-base64") patch[2].value.KARS_PROVIDER_OPENAI_API_KEY = "not base64!";
    if (operation === "empty-data") patch[2].value = {};
    if (operation === "extra-operation") patch.push({ op: "remove", path: "/metadata/finalizers" });
    input = operation === "missing-input" ? undefined : operation === "malformed-json" ? "{" : JSON.stringify(patch);
    await expect(f.run(write => write(args, input))).rejects.toThrow("only one exact provider Secret");
    expect(f.local).toHaveBeenCalledTimes(1); f.cleaned();
  });
  it("rejects a shared or symlinked parent directory without issuing a token", async () => {
    const f = fixture(); chmodSync(f.directory, 0o755);
    await expect(f.run(vi.fn())).rejects.toThrow("owned private directory"); chmodSync(f.directory, 0o700);
    const link = path.join(f.directory, "link"); symlinkSync(f.directory, link);
    await expect(withKindBetaInstallerWriter(f.execute, f.writer, link, tools, f.local, vi.fn())).rejects.toThrow("owned private directory");
    expect(f.execute.mock.calls.some(([args]) => args[1] === "--raw")).toBe(false);
  });
});
