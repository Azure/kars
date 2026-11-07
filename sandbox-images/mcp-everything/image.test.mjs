// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { fork } from "node:child_process";
import { randomUUID } from "node:crypto";
import { once } from "node:events";
import { readFile } from "node:fs/promises";
import { Server } from "node:net";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { Hono } from "hono";
import { serve } from "@hono/node-server";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { WebStandardStreamableHTTPServerTransport } from "@modelcontextprotocol/sdk/server/webStandardStreamableHttp.js";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import { LATEST_PROTOCOL_VERSION } from "@modelcontextprotocol/sdk/types.js";

const directory = new URL(".", import.meta.url);
const readJson = async path => JSON.parse(await readFile(new URL(path, directory), "utf8"));

if (process.argv.includes("--server-fixture")) {
  // The unmodified upstream launcher binds all interfaces; tests must be loopback-only.
  const listen = Server.prototype.listen;
  Server.prototype.listen = function (...args) {
    const callback = args.findLast(value => typeof value === "function");
    this.once("listening", () => process.send({ port: this.address().port }));
    return listen.call(this, 0, "127.0.0.1", callback);
  };
  await import("./node_modules/@modelcontextprotocol/server-everything/dist/transports/streamableHttp.js");
} else {
  test("the image preserves AL3, build-only installation and non-root startup", async () => {
    const dockerfile = await readFile(new URL("Dockerfile", directory), "utf8");
    assert.match(dockerfile, /^FROM mcr\.microsoft\.com\/azurelinux\/base\/nodejs:24@sha256:[a-f0-9]{64}$/m);
    assert.match(dockerfile, /RUN npm ci --omit=dev --ignore-scripts/);
    assert.match(dockerfile, /^USER 1000$/m);
    assert.match(dockerfile, /^ENTRYPOINT \["\.\/node_modules\/\.bin\/mcp-server-everything"\]$/m);
    assert.match(dockerfile, /^CMD \["streamableHttp"\]$/m);
  });

  test("the locked and installed reference server use the patched SDK", async () => {
    const manifest = await readJson("package.json");
    const lock = await readJson("package-lock.json");
    const installed = await readJson("node_modules/@modelcontextprotocol/sdk/package.json");
    assert.equal(manifest.dependencies["@modelcontextprotocol/server-everything"], "2026.7.4");
    assert.equal(manifest.overrides["@modelcontextprotocol/sdk"], "1.32.0");
    assert.equal(lock.packages["node_modules/@modelcontextprotocol/sdk"].version, "1.32.0");
    assert.equal(installed.version, "1.32.0");
    assert.equal(
      lock.packages["node_modules/@modelcontextprotocol/sdk"].resolved,
      "https://registry.npmjs.org/@modelcontextprotocol/sdk/-/sdk-1.32.0.tgz",
    );
  });

  test("audited dependency patches remain installed with strong public-registry bindings", async () => {
    const manifest = await readJson("package.json");
    const lock = await readJson("package-lock.json");
    for (const [name, version] of Object.entries({
      "@hono/node-server": "2.1.3", "fast-uri": "3.1.9", hono: "4.13.11",
      "ip-address": "10.7.4", "proxy-addr": "2.0.8", qs: "6.16.1",
    })) {
      assert.equal(manifest.overrides[name], version);
      const copies = Object.entries(lock.packages).filter(([path]) => path.endsWith(`node_modules/${name}`));
      assert.ok(copies.length > 0, name);
      for (const [path, metadata] of copies) {
        assert.equal(metadata.version, version, path);
        assert.equal((await readJson(`${path}/package.json`)).version, version, path);
      }
    }
    for (const [name, metadata] of Object.entries(lock.packages)) {
      if (!name) continue;
      const url = new URL(metadata.resolved);
      assert.equal(url.origin, "https://registry.npmjs.org", name);
      assert.deepEqual([url.username, url.password, url.search, url.hash], ["", "", "", ""], name);
      assert.match(url.pathname, /\.tgz$/, name);
      assert.match(metadata.integrity, /^sha512-[A-Za-z0-9+/]+={0,2}$/, name);
    }
  });

  test("the SDK web transport works through Hono and the supported Node adapter v2", async t => {
    const sdk = new McpServer({ name: "hono-compatibility", version: "1.0.0" });
    sdk.registerTool("check", { description: "Local compatibility probe" }, async () => ({
      content: [{ type: "text", text: "hono-node-v2-compatible" }],
    }));
    const transport = new WebStandardStreamableHTTPServerTransport({
      enableJsonResponse: true, sessionIdGenerator: randomUUID,
    });
    await sdk.connect(transport);
    const app = new Hono();
    app.all("/mcp", context => transport.handleRequest(context.req.raw));
    const server = serve({ fetch: app.fetch, hostname: "127.0.0.1", port: 0 });
    t.after(async () => {
      await sdk.close();
      server.closeAllConnections();
      await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
    });
    if (!server.listening) await once(server, "listening", { signal: AbortSignal.timeout(5000) });
    const address = server.address();
    assert.ok(address && typeof address !== "string");
    const client = new Client({ name: "hono-client", version: "1.0.0" });
    t.after(() => client.close());
    await client.connect(new StreamableHTTPClientTransport(new URL(`http://127.0.0.1:${address.port}/mcp`)));
    assert.equal(client.getServerVersion().name, "hono-compatibility");
    assert.ok((await client.listTools()).tools.some(tool => tool.name === "check"));
    const result = await client.callTool({ name: "check", arguments: {} });
    assert.notEqual(result.isError, true);
    assert.deepEqual(result.content, [{ type: "text", text: "hono-node-v2-compatible" }]);
  });

  test("the actual Everything HTTP transport remains compatible", async t => {
    const child = fork(fileURLToPath(import.meta.url), ["--server-fixture"], {
      cwd: fileURLToPath(directory),
      execArgv: [],
      stdio: ["ignore", "pipe", "pipe", "ipc"],
    });
    const exited = once(child, "exit");
    let outputBytes = 0;
    for (const stream of [child.stdout, child.stderr]) {
      stream.on("data", data => {
        outputBytes += data.length;
        if (outputBytes > 65536) child.kill("SIGKILL");
      });
    }
    t.after(async () => {
      if (child.exitCode === null && child.signalCode === null) {
        child.kill("SIGTERM");
        const timer = setTimeout(() => child.kill("SIGKILL"), 2000);
        try {
          await exited;
        } finally {
          clearTimeout(timer);
        }
      }
      assert.ok(outputBytes <= 65536, "Reference server exceeded its log bound");
    });
    const [{ port }] = await once(child, "message", { signal: AbortSignal.timeout(5000) });
    assert.ok(Number.isInteger(port) && port > 0 && port <= 65535);
    const url = new URL(`http://127.0.0.1:${port}/mcp`);
    const transport = new StreamableHTTPClientTransport(url);
    const client = new Client({ name: "kars-image-regression", version: "0.1.0" });
    t.after(() => client.close());
    await client.connect(transport);
    assert.equal(client.getServerVersion().name, "mcp-servers/everything");
    const session = transport.sessionId;
    assert.ok(session);
    const headers = {
      "Content-Type": "application/json",
      Accept: "application/json, text/event-stream",
      "mcp-session-id": session,
      "mcp-protocol-version": LATEST_PROTOCOL_VERSION,
    };
    const body = JSON.stringify({ jsonrpc: "2.0", id: 100, method: "tools/list", params: {} });

    await t.test("discovers and invokes the real echo tool", async () => {
      const catalog = await client.listTools();
      assert.ok(catalog.tools.some(tool => tool.name === "echo"));
      const result = await client.callTool({
        name: "echo", arguments: { message: "kars-sdk-compatibility" },
      });
      assert.notEqual(result.isError, true);
      assert.ok(result.content.some(item =>
        item.type === "text" && item.text.includes("kars-sdk-compatibility")));
    });
    await t.test("rejects JSON-only Accept rather than concealing a protocol failure", async () => {
      const response = await fetch(url, {
        method: "POST", headers: { ...headers, Accept: "application/json" }, body,
        signal: AbortSignal.timeout(5000),
      });
      await response.body.cancel();
      assert.equal(response.status, 406);
    });
    await t.test("terminates the actual session and rejects its reuse", async () => {
      await transport.terminateSession();
      const response = await fetch(url, {
        method: "POST", headers, body, signal: AbortSignal.timeout(5000),
      });
      await response.body.cancel();
      assert.equal(response.status, 400);
    });
  });
}
