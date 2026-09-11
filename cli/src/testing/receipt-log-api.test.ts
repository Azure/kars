// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { spawn } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { expect, it } from "vitest";
import { inclusionEntryHash } from "../commands/receipt-log.js";

const root = fileURLToPath(new URL("../../../", import.meta.url));
const namespace = "receipt-api-test";
const sentinel = "PRIVATE-API-ERROR-BODY";

async function fixture(failure: boolean) {
  const requests: string[] = [];
  const entry = { seq: 0, receipt: "ns/task", payloadSha256: "digest", prevHash: "genesis",
    entryHash: inclusionEntryHash(0, "ns/task", "digest", "genesis") };
  const server = createServer((request, response) => {
    const path = new URL(request.url ?? "/", "http://127.0.0.1").pathname;
    requests.push(`${request.method} ${path}`);
    const send = (code: number, body: unknown) => {
      response.writeHead(code, { "Content-Type": "application/json" });
      response.end(JSON.stringify(body));
    };
    if (request.method !== "GET") return send(405, {});
    if (path === "/api") return send(200, { kind: "APIVersions", apiVersion: "v1",
      versions: ["v1"], serverAddressByClientCIDRs: [] });
    if (path === "/apis") return send(200, { kind: "APIGroupList", apiVersion: "v1", groups: [] });
    if (path === "/api/v1") return send(200, { kind: "APIResourceList", apiVersion: "v1",
      groupVersion: "v1", resources: [{ name: "configmaps", singularName: "configmap",
        namespaced: true, kind: "ConfigMap", verbs: ["get", "list"] }] });
    if (path === `/api/v1/namespaces/${namespace}/configmaps`) {
      if (failure) return send(403, { kind: "Status", apiVersion: "v1", status: "Failure",
        code: 403, reason: "Forbidden", message: sentinel });
      return send(200, { kind: "ConfigMapList", apiVersion: "v1",
        metadata: { resourceVersion: "42" }, items: [{
          kind: "ConfigMap", apiVersion: "v1",
          metadata: { name: "kars-receipt-log", namespace, uid: "head-uid", resourceVersion: "40" },
          data: { "chain.json": JSON.stringify([entry]) },
        }] });
    }
    return send(404, { kind: "Status", apiVersion: "v1", status: "Failure",
      code: 404, reason: "NotFound" });
  });
  await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("Controlled API listener unavailable");
  const directory = await mkdtemp(join(tmpdir(), "kars-receipt-cli-"));
  const kubeconfig = join(directory, "config.json");
  await writeFile(kubeconfig, JSON.stringify({ apiVersion: "v1", kind: "Config",
    "current-context": "owned-receipt-api",
    clusters: [{ name: "owned", cluster: { server: `http://127.0.0.1:${address.port}` } }],
    users: [{ name: "anonymous", user: {} }],
    contexts: [{ name: "owned-receipt-api", context: { cluster: "owned", user: "anonymous" } }],
  }), { mode: 0o600 });
  return { requests, entry, kubeconfig, close: async () => {
    server.closeAllConnections();
    await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
    await rm(directory, { recursive: true });
  } };
}

function run(kubeconfig: string): Promise<{ code: number | null; stdout: string; stderr: string }> {
  return new Promise((resolve, reject) => {
    const process = spawn(globalThis.process.execPath, [
      join(root, "cli/dist/index.js"), "receipt", "log", "--format", "json",
    ], { cwd: root, env: { ...globalThis.process.env, KUBECONFIG: kubeconfig,
      KARS_NAMESPACE: namespace, NO_COLOR: "1" }, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "", stderr = "";
    process.stdout.setEncoding("utf8").on("data", chunk => { stdout += chunk; });
    process.stderr.setEncoding("utf8").on("data", chunk => { stderr += chunk; });
    const timeout = setTimeout(() => process.kill("SIGTERM"), 20_000);
    process.on("error", error => { clearTimeout(timeout); reject(error); });
    process.on("close", code => { clearTimeout(timeout); resolve({ code, stdout, stderr }); });
  });
}

it("reads the actual kubectl snapshot through the packaged receipt command", async () => {
  const api = await fixture(false);
  try {
    const result = await run(api.kubeconfig);
    expect(result.stderr).toBe("");
    expect(result.code).toBe(0);
    expect(JSON.parse(result.stdout)).toEqual({ entries: [api.entry], intact: true, brokenAt: null });
    expect(api.requests.filter(path => path.endsWith("/configmaps"))).toEqual([
      `GET /api/v1/namespaces/${namespace}/configmaps`,
    ]);
  } finally {
    await api.close();
  }
}, 30_000);

it("makes actual API denial nonzero without printing private server error bodies", async () => {
  const api = await fixture(true);
  try {
    const result = await run(api.kubeconfig);
    expect(result.code).not.toBe(0);
    expect(result.stderr).toContain("Unable to read receipt log ConfigMaps");
    expect(result.stdout).not.toContain("intact");
    expect(result.stderr + result.stdout).not.toContain(sentinel);
    expect(api.requests.some(path => path.endsWith("/configmaps"))).toBe(true);
  } finally {
    await api.close();
  }
}, 30_000);
