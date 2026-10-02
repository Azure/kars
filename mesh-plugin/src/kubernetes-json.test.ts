// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createServer, type Server } from "node:https";
import type { IncomingMessage, ServerResponse } from "node:http";
import { InClusterKubernetes, KubernetesError } from "./kubernetes-json.js";

let directory: string;
let endpoint: string;
let server: Server;
let handler: (req: IncomingMessage, res: ServerResponse) => void;
const client = (timeout = 1000) => new InClusterKubernetes(endpoint, join(directory, "token"), join(directory, "cert.pem"), timeout);

beforeAll(async () => {
  directory = mkdtempSync(join(tmpdir(), "kars-kubernetes-json-"));
  writeFileSync(join(directory, "openssl.cnf"), "[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n[dn]\nCN=kars-test.invalid\n[ext]\nsubjectAltName=IP:127.0.0.1\nbasicConstraints=critical,CA:TRUE\n");
  execFileSync("openssl", ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-config", join(directory, "openssl.cnf"), "-keyout", join(directory, "key.pem"), "-out", join(directory, "cert.pem")], { stdio: "ignore" });
  writeFileSync(join(directory, "token"), "test-token", { mode: 0o600 });
  server = createServer({ key: readFileSync(join(directory, "key.pem")), cert: readFileSync(join(directory, "cert.pem")) }, (req, res) => handler(req, res));
  await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("No test listener");
  endpoint = `https://127.0.0.1:${address.port}`;
});
afterAll(async () => {
  if (server) { server.closeAllConnections(); await new Promise<void>(resolve => server.close(() => resolve())); }
  if (directory) rmSync(directory, { recursive: true, force: true });
});

describe("in-cluster Kubernetes HTTPS client", () => {
  it("uses verified TLS and rereads rotated service-account credentials", async () => {
    const tokens: Array<string | undefined> = [];
    handler = (req, res) => { tokens.push(req.headers.authorization); res.end('{"ok":true}'); };
    const api = client();
    expect(await api.request("GET", "/api/v1/namespaces?limit=1")).toEqual({ ok: true });
    writeFileSync(join(directory, "token"), "rotated-test-token");
    await api.request("GET", "/api/v1/namespaces");
    expect(tokens).toEqual(["Bearer test-token", "Bearer rotated-test-token"]);
  });
  it.each(["//other.test/path", "/\\other.test/path", "/path#fragment", "/path\nnext", "/path\u0000", "/path with space"])("rejects unsafe paths before credentials are read: %j", async path => {
    await expect(new InClusterKubernetes(endpoint, "/missing-token", "/missing-ca").request("GET", path)).rejects.toThrow("Invalid Kubernetes path");
  });
  it.each(["http://localhost", "https://user:pass@localhost", "https://localhost/path", "https://localhost?q=1"])("rejects non-origin endpoint %s", origin => {
    expect(() => new InClusterKubernetes(origin)).toThrow("HTTPS origin");
  });
  it("requires an in-cluster host instead of resolving an undefined hostname", () => {
    vi.stubEnv("KUBERNETES_SERVICE_HOST", "");
    try { expect(() => new InClusterKubernetes()).toThrow("KUBERNETES_SERVICE_HOST"); }
    finally { vi.unstubAllEnvs(); }
  });
  it("does not follow redirects or expose response bodies in errors", async () => {
    let calls = 0;
    handler = (_req, res) => { calls++; res.writeHead(302, { location: `${endpoint}/redirected` }); res.end("sensitive-test-body"); };
    const error = await client().request("GET", "/redirect").catch(e => e);
    expect(error).toBeInstanceOf(KubernetesError);
    if (!(error instanceof KubernetesError)) throw new Error("Expected Kubernetes HTTP error");
    expect(error.status).toBe(302);
    expect(error.message).not.toContain("sensitive-test-body");
    expect(calls).toBe(1);
  });
  it("returns undefined for an empty HTTP 204", async () => {
    handler = (_req, res) => { res.writeHead(204); res.end(); };
    expect(await client().request("DELETE", "/api/v1/namespaces/test")).toBeUndefined();
  });
  it("preserves merge-patch content type and JSON body", async () => {
    let type: string | undefined; let body = "";
    handler = (req, res) => { type = req.headers["content-type"]; req.on("data", chunk => { body += chunk; }); req.on("end", () => res.end(body)); };
    const patch = { metadata: { resourceVersion: "2" } };
    expect(await client().request("PATCH", "/api/v1/test", patch, "application/merge-patch+json")).toEqual(patch);
    expect(type).toBe("application/merge-patch+json");
  });
  it("bounds an unresponsive request", async () => {
    handler = () => {};
    await expect(client(30).request("GET", "/slow")).rejects.toThrow("deadline exceeded");
  });
  it("rejects aborted and oversized responses", async () => {
    handler = (_req, res) => { res.writeHead(200, { "content-length": "100" }); res.write("{"); res.socket?.destroy(); };
    await expect(client().request("GET", "/aborted")).rejects.toThrow();
    handler = (_req, res) => res.end("x".repeat(2 * 1024 * 1024 + 1));
    await expect(client().request("GET", "/oversized")).rejects.toThrow("exceeds 2 MiB");
  });
  it("bounds request JSON before opening a connection", async () => {
    const called = vi.fn(); handler = called;
    await expect(client().request("POST", "/large", { text: "x".repeat(1024 * 1024) })).rejects.toThrow("exceeds 1 MiB");
    expect(called).not.toHaveBeenCalled();
  });
  it("rejects invalid successful JSON", async () => {
    handler = (_req, res) => res.end("not-json");
    await expect(client().request("GET", "/invalid-json")).rejects.toBeInstanceOf(SyntaxError);
  });
  it("enforces the overall deadline even while a response keeps streaming", async () => {
    handler = (_req, res) => {
      res.writeHead(200); res.write("{");
      const interval = setInterval(() => res.write(" "), 5);
      res.on("close", () => clearInterval(interval));
    };
    await expect(client(50).request("GET", "/trickle")).rejects.toThrow("deadline exceeded");
  });
  it("does not disable TLS certificate hostname verification", async () => {
    handler = (_req, res) => res.end("{}");
    const mismatched = new InClusterKubernetes(endpoint.replace("127.0.0.1", "localhost"), join(directory, "token"), join(directory, "cert.pem"), 1000);
    await expect(mismatched.request("GET", "/tls")).rejects.toMatchObject({ code: "ERR_TLS_CERT_ALTNAME_INVALID" });
  });
});
