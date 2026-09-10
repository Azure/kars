// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { spawn } from "node:child_process";

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const loopback = host => ["localhost", "127.0.0.1", "[::1]"].includes(host);

export class BudgetApiError extends Error {
  constructor(category, httpStatus = 0) {
    super(`Budget fixture API ${category}`);
    this.category = category;
    this.httpStatus = httpStatus;
  }
}

export function apiRequest(base, deadline = Infinity) {
  const origin = new URL(base);
  assert(origin.protocol === "http:" && loopback(origin.hostname) && !origin.username && !origin.password
    && origin.pathname === "/" && !origin.search && !origin.hash, "Exact loopback API proxy required");
  return async (method, path, body, actor) => {
    assert(["GET", "POST", "PUT", "PATCH", "DELETE"].includes(method), "Unexpected fixture API verb");
    const url = new URL(path, origin);
    assert(path.startsWith("/") && url.origin === origin.origin, "Fixture API path changed origin");
    const remaining = Math.min(15_000, deadline - Date.now());
    if (remaining <= 0) throw new BudgetApiError("deadline");
    let response;
    try {
      response = await fetch(url, {
        method, signal: AbortSignal.timeout(Math.ceil(remaining)), redirect: "error",
        headers: {
          "Content-Type": method === "PATCH" ? "application/merge-patch+json" : "application/json",
          Accept: "application/json",
          ...(actor ? { "Impersonate-User": actor, "Impersonate-Group": "system:authenticated" } : {}),
        },
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      });
    } catch {
      throw new BudgetApiError("transport");
    }
    const chunks = [];
    let size = 0;
    try {
      for await (const chunk of response.body ?? []) {
        size += chunk.length;
        if (size > 1024 * 1024) throw new BudgetApiError("response-size", response.status);
        chunks.push(chunk);
      }
    } catch (error) {
      if (error instanceof BudgetApiError) throw error;
      throw new BudgetApiError("response-transport", response.status);
    }
    let value;
    try { value = JSON.parse(Buffer.concat(chunks).toString("utf8")); }
    catch { throw new BudgetApiError("response-json", response.status); }
    return { status: response.status, body: value };
  };
}

export async function withKindApi({ root, context, kubectl, deadline = Infinity, spawnProcess = spawn }, run) {
  assert(["kind-kars-e2e", "kind-kars-budget-api"].includes(context), "Owned budget Kind context required");
  let config, host;
  try {
    config = JSON.parse(kubectl(["config", "view", "--minify", "-o", "json"]));
    host = new URL(config.clusters?.[0]?.cluster?.server).hostname;
  } catch { throw new BudgetApiError("proxy-config"); }
  assert(config.contexts?.length === 1 && config.contexts[0].name === context
    && config.clusters?.length === 1 && loopback(host),
  "Budget API requires the exact loopback Kind context");
  if (Date.now() >= deadline) throw new BudgetApiError("deadline");
  const proxy = spawnProcess("kubectl", ["--context", context, "--request-timeout=20s", "proxy",
    "--address=127.0.0.1", "--port=0"], { cwd: root, stdio: ["ignore", "pipe", "pipe"] });
  let output = "", failed = false;
  proxy.stdout.on("data", data => { output = (output + data).slice(-2048); });
  proxy.stderr.on("data", () => {});
  proxy.on("error", () => { failed = true; });
  const exited = () => proxy.exitCode !== null || proxy.signalCode !== null;
  const invoke = async () => {
    const startup = Math.min(deadline, Date.now() + 30_000);
    while (Date.now() < startup) {
      if (failed || exited()) throw new BudgetApiError("proxy-exited");
      const port = output.match(/Starting to serve on 127\.0\.0\.1:(\d+)/)?.[1];
      if (port) return await run(apiRequest(`http://127.0.0.1:${port}`, deadline));
      await sleep(Math.min(50, startup - Date.now()));
    }
    throw new BudgetApiError("proxy-deadline");
  };
  const stop = async () => {
    if (!exited() && proxy.pid) {
      proxy.kill("SIGTERM");
      const grace = Date.now() + 2000;
      while (!exited() && Date.now() < grace) await sleep(25);
      if (!exited()) {
        proxy.kill("SIGKILL");
        const killed = Date.now() + 2000;
        while (!exited() && Date.now() < killed) await sleep(25);
        if (!exited()) throw new BudgetApiError("proxy-cleanup");
      }
    }
  };
  let result, failure, rejected = false;
  try { result = await invoke(); }
  catch (error) { failure = error; rejected = true; }
  try { await stop(); }
  catch (error) {
    throw new AggregateError(rejected ? [failure, error] : [error], "Budget fixture API proxy cleanup failed");
  }
  if (rejected) throw failure;
  return result;
}
