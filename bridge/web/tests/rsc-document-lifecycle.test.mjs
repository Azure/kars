// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const source = ts.transpileModule(readFileSync(
  new URL("../src/instrumentation-client.ts", import.meta.url), "utf8",
), { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;

function harness(result = () => Promise.resolve(new Response("flight"))) {
  const calls = [];
  const window = {
    location: new URL("https://bridge.example/console/insights"),
    fetch(input, init) {
      assert.equal(this, window);
      calls.push({ input, init });
      return result();
    },
  };
  runInNewContext(source, { window, Request, Headers, URL });
  return { fetch: window.fetch, calls };
}

test("same-origin RSC reads survive document unload without changing request authority", async () => {
  for (const input of ["/console/fleet?_rsc=opaque", new URL("https://bridge.example/console/fleet")]) {
    const h = harness();
    const controller = new AbortController();
    const init = {
      headers: { RSC: "1", "Next-Router-State-Tree": "existing" },
      credentials: "same-origin", cache: "no-store", signal: controller.signal,
      redirect: "follow", priority: "auto",
    };
    const response = await h.fetch(input, init);
    assert.equal(await response.text(), "flight");
    assert.equal(h.calls[0].input, input);
    for (const [key, value] of Object.entries(init)) assert.equal(h.calls[0].init[key], value);
    assert.equal(h.calls[0].init.keepalive, true);
    assert.equal("keepalive" in init, false);
  }
});

test("Request inputs preserve their headers, credentials and cancellation signal", async () => {
  const h = harness();
  const input = new Request("https://bridge.example/console/fleet", {
    headers: { rsc: "1" }, credentials: "include", signal: new AbortController().signal,
  });
  await h.fetch(input);
  assert.equal(h.calls[0].input, input);
  assert.equal(h.calls[0].init.keepalive, true);
  assert.deepEqual(Object.keys(h.calls[0].init), ["keepalive"]);
});

test("API calls, mutations, foreign origins and non-RSC requests are unchanged", async () => {
  for (const [input, init] of [
    ["/api/operator/datapath-witness", undefined],
    ["/console/fleet", { headers: { rsc: "0" } }],
    ["/console/fleet", { headers: { rsc: "1" }, method: "POST", body: "action" }],
    ["https://identity.example/login", { headers: { rsc: "1" } }],
    [new Request("https://bridge.example/console/fleet", { headers: { rsc: "1" } }), { headers: {} }],
    [new Request("https://bridge.example/console/fleet", { headers: { rsc: "1" }, method: "POST" }), undefined],
  ]) {
    const h = harness();
    await h.fetch(input, init);
    assert.equal(h.calls[0].input, input);
    assert.equal(h.calls[0].init, init);
  }
});

test("genuine network failures and explicit cancellation are not swallowed or retried", async () => {
  for (const error of [new TypeError("network unavailable"), new DOMException("cancelled", "AbortError")]) {
    const h = harness(() => Promise.reject(error));
    await assert.rejects(h.fetch("/console/fleet", { headers: { rsc: "1" } }), value => value === error);
    assert.equal(h.calls.length, 1);
  }
});

test("authentication and server failures retain the original response", async () => {
  for (const status of [401, 403, 503]) {
    const response = new Response("unavailable", { status });
    const h = harness(() => Promise.resolve(response));
    assert.equal(await h.fetch("/console/fleet", { headers: { rsc: "1" } }), response);
  }
});
