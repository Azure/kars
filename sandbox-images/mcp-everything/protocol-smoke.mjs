// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { createConnection, createServer } from "node:net";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";

const directory = fileURLToPath(new URL(".", import.meta.url));
const reservation = createServer();
reservation.listen(0, "127.0.0.1");
await once(reservation, "listening");
const port = reservation.address().port;
await new Promise((resolve) => reservation.close(resolve));
const child = spawn(process.execPath, [
  "node_modules/.bin/mcp-server-everything", "streamableHttp",
], { cwd: directory, env: { ...process.env, PORT: String(port) }, stdio: "ignore" });
let childFailed = false;
child.on("error", () => { childFailed = true; });
const exited = new Promise((resolve) => {
  child.once("exit", resolve);
  child.once("error", resolve);
});
const client = new Client({ name: "kars-dependency-smoke", version: "1.0.0" });
const transport = new StreamableHTTPClientTransport(new URL(`http://127.0.0.1:${port}/mcp`));
let stage = "startup";
let timer;
let stopped = false;
let clientClosed = false;

async function exercise() {
  while (true) {
    assert.ok(!stopped && !childFailed && child.exitCode === null && child.signalCode === null,
      "Everything process stopped");
    const listening = await new Promise((resolve) => {
      const socket = createConnection({ host: "127.0.0.1", port });
      const done = (ready) => { socket.destroy(); resolve(ready); };
      socket.once("connect", () => done(true));
      socket.once("error", () => done(false));
      socket.setTimeout(500, () => done(false));
    });
    if (listening) break;
    await delay(100);
  }
  stage = "initialize";
  await client.connect(transport);
  stage = "tools-list";
  const catalog = await client.listTools();
  assert.ok(catalog.tools.some((tool) => tool.name === "echo"));
  stage = "echo";
  const marker = "kars-dependency-smoke";
  const result = await client.callTool({ name: "echo", arguments: { message: marker } });
  assert.notEqual(result.isError, true);
  assert.ok(result.content.some((item) => item.type === "text" && item.text.includes(marker)));
  stage = "session-close";
  await transport.terminateSession();
  await client.close();
  clientClosed = true;
  assert.ok(!childFailed && child.exitCode === null && child.signalCode === null,
    "Everything process exited during the protocol check");
  return { initialized: true, toolsListed: true, echoPassed: true, sessionClosed: true,
    toolCount: catalog.tools.length };
}

let result;
try {
  result = await Promise.race([
    exercise(),
    new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error("Protocol smoke deadline exceeded")), 45_000);
    }),
  ]);
} catch {
  console.error(`Everything protocol smoke failed at ${stage}; no server output/body logged`);
  process.exitCode = 1;
} finally {
  stopped = true;
  clearTimeout(timer);
  if (!clientClosed) {
    try {
      const closed = await Promise.race([
        client.close().then(() => true),
        delay(1_000).then(() => false),
      ]);
      assert.ok(closed, "Client cleanup deadline exceeded");
    } catch {
      console.error("Everything protocol smoke client cleanup failed; no response/body logged");
      process.exitCode = 1;
    }
  }
  child.kill("SIGTERM");
  await Promise.race([exited, delay(2_000)]);
  if (!childFailed && child.exitCode === null && child.signalCode === null) {
    child.kill("SIGKILL");
    await Promise.race([exited, delay(1_000)]);
  }
  if (!childFailed && child.exitCode === null && child.signalCode === null) {
    console.error("Everything protocol smoke child did not stop");
    process.exitCode = 1;
  }
}
if (!process.exitCode) console.log(JSON.stringify(result));
