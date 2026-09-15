// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { test } from "node:test";
import { readFileSync } from "node:fs";
import { createServer } from "node:http";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import * as login from "../src/lib/copilot-login.ts";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const contract = JSON.parse(readFileSync(new URL("../../contracts/copilot-login.json", import.meta.url)));
const PRIVATE = "PRIVATE_VALUE_NEVER_ECHOED";
const flush = async () => { for (let n = 0; n < 12; n++) await Promise.resolve(); };
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};

function clock() {
  let now = 0, sequence = 0;
  const timers = new Map();
  return {
    now: () => now,
    setTimeout(callback, ms) { const id = ++sequence; timers.set(id, { at: now + ms, callback }); return id; },
    clearTimeout(id) { timers.delete(id); },
    async advance(ms) {
      const until = now + ms;
      while (true) {
        const next = [...timers.entries()].sort((a, b) => a[1].at - b[1].at)[0];
        if (!next || next[1].at > until) break;
        now = next[1].at;
        timers.delete(next[0]);
        next[1].callback();
        await flush();
      }
      now = until;
      await flush();
    },
    deadlines: () => [...timers.values()].map(t => t.at).sort((a, b) => a - b),
  };
}

function ui() {
  const state = { flow: null, starting: false, error: null, pending: undefined, authorized: [] };
  const events = [];
  return {
    state, events,
    callbacks: Object.fromEntries(["flow", "starting", "error", "pending", "authorized"].map(key => [
      key, value => { events.push(key); if (key === "authorized") state.authorized.push(value); else state[key] = value; },
    ])),
  };
}

function controller(poll, options = {}) {
  const time = clock();
  const view = ui();
  const calls = [];
  const service = {
    start: options.start ?? (async () => ({ ok: true, data: contract.start })),
    poll: async (device, interval) => { calls.push({ device, interval, at: time.now() }); return poll(device, interval); },
  };
  const control = login.createCopilotLoginController(service, view.callbacks, time);
  return { time, view, calls, control };
}

for (const outcome of ["authorized", "storage_unconfirmed", "transport_rejected"]) {
  test(`deadline with an unresolved poll retains an unconfirmed outcome after late ${outcome}`, async () => {
    const pending = deferred();
    const h = controller(() => pending.promise);
    await h.control.begin();
    await h.time.advance(39_999);
    assert.equal(h.calls.length, 1);
    assert.ok(h.view.state.flow);
    await h.time.advance(1);
    assert.equal(h.view.state.flow, null);
    assert.equal(h.view.state.error, login.copilotErrors.copilot_outcome_unconfirmed);
    assert.match(h.view.state.error, /Inspect provider state before retrying/);
    assert.notEqual(h.view.state.error, login.copilotErrors.copilot_expired);
    const state = JSON.stringify(h.view.state), events = h.view.events.length;
    if (outcome === "transport_rejected") pending.reject(new Error(PRIVATE));
    else pending.resolve(outcome === "authorized" ? { ...contract.authorized, access_token: PRIVATE }
      : { status: "error", error: login.copilotErrors.copilot_storage_unconfirmed, token: PRIVATE });
    await flush();
    await h.time.advance(900_000);
    assert.equal(JSON.stringify(h.view.state), state);
    assert.equal(h.view.events.length, events);
    assert.equal(JSON.stringify(h.view.state).includes(PRIVATE), false);
    assert.equal(h.view.state.authorized.length, 0);
    assert.equal(h.calls.length, 1);
    assert.deepEqual(h.time.deadlines(), []);
  });
}

test("deadline without an in-flight poll remains definitive expiry and stops all polling", async () => {
  const h = controller(async () => contract.pending);
  await h.control.begin();
  await h.time.advance(39_999);
  assert.equal(h.calls.length, 7);
  await h.time.advance(1);
  assert.equal(h.view.state.flow, null);
  assert.equal(h.view.state.error, login.copilotErrors.copilot_expired);
  const events = h.view.events.length;
  await h.time.advance(900_000);
  assert.equal(h.view.events.length, events);
  assert.equal(h.calls.length, 7);
  assert.equal(h.view.state.authorized.length, 0);
  assert.deepEqual(h.time.deadlines(), []);
});

test("an explicit upstream expired_token result remains definitive before the local deadline", async () => {
  const h = controller(async () => ({ status: "error", error: login.copilotErrors.copilot_expired }));
  await h.control.begin();
  await h.time.advance(5000);
  assert.equal(h.view.state.flow, null);
  assert.equal(h.view.state.error, login.copilotErrors.copilot_expired);
  const events = h.view.events.length;
  await h.time.advance(900_000);
  assert.equal(h.view.events.length, events);
  assert.equal(h.calls.length, 1);
  assert.equal(h.view.state.authorized.length, 0);
  assert.deepEqual(h.time.deadlines(), []);
});

test("slowdown without a supplied interval is cumulative, bounded and never resets expiry", async () => {
  const h = controller(async () => ({ status: "pending", reason: "slow_down" }));
  await h.control.begin();
  await h.time.advance(40_000);
  assert.deepEqual(h.calls.map(c => [c.at, c.interval]), [[5000, 5], [15000, 10], [30000, 15]]);
  assert.equal(h.view.state.flow, null);
  assert.equal(h.view.state.error, login.copilotErrors.copilot_expired);
  assert.deepEqual(h.time.deadlines(), []);
});

test("legacy pending and missing models remain compatible without claiming approval is pending", async () => {
  const h = controller(async () => contract.pending_legacy);
  await h.control.begin();
  await h.time.advance(15_000);
  assert.deepEqual(h.calls.map(c => c.interval), [5, 5, 5]);
  assert.equal(h.view.state.pending, undefined);
  h.control.cancel();
  assert.equal(login.parseCopilotPoll({ status: "authorized" }).status, "authorized");
});

test("transport rejection clears the active flow without disclosing error contents", async () => {
  const h = controller(async () => { throw new Error(PRIVATE); });
  await h.control.begin();
  await h.time.advance(5000);
  assert.equal(h.view.state.flow, null);
  assert.equal(h.view.state.error, login.copilotErrors.transport);
  assert.equal(JSON.stringify(h.view.state).includes(PRIVATE), false);
  await h.time.advance(900_000);
  assert.equal(h.calls.length, 1);
});

for (const operation of ["cancel", "dispose", "replace"]) {
  for (const phase of ["start", "poll"]) {
    test(`${operation} ignores late ${phase} completion and rejection`, async () => {
      for (const rejected of [false, true]) {
        const old = deferred();
        let starts = 0;
        const h = controller(() => old.promise, {
          start: async () => ++starts === 1 && phase === "start" ? old.promise : { ok: true, data: contract.start },
        });
        const beginning = h.control.begin();
        if (phase === "poll") { await beginning; await h.time.advance(5000); }
        if (operation === "replace") await h.control.begin();
        else h.control[operation]();
        const before = JSON.stringify(h.view.state), count = h.view.events.length;
        if (rejected) old.reject(new Error(PRIVATE));
        else old.resolve(phase === "start" ? { ok: true, data: contract.start } : contract.authorized);
        await beginning;
        await flush();
        assert.equal(JSON.stringify(h.view.state), before);
        assert.equal(h.view.events.length, count);
        h.control.dispose();
      }
    });
  }
}

test("start time is included in expiry and malformed starts never create an active flow", async () => {
  const start = deferred();
  const h = controller(async () => contract.pending, { start: () => start.promise });
  const beginning = h.control.begin();
  await h.time.advance(39_000);
  start.resolve({ ok: true, data: contract.start });
  await beginning;
  await h.time.advance(1000);
  assert.equal(h.calls.length, 0);
  assert.equal(h.view.state.error, login.copilotErrors.copilot_expired);
  for (const data of [{}, { ...contract.start, verification_uri: `https://untrusted.example/${PRIVATE}` }]) {
    const bad = controller(async () => contract.pending, { start: async () => ({ ok: true, data }) });
    await bad.control.begin();
    assert.equal(bad.view.state.flow, null);
    assert.equal(bad.view.state.error, login.copilotErrors.copilot_invalid_response);
  }
});

for (const input of [
  {}, null, [], { status: PRIVATE }, { status: "pending", interval: 0 },
  { status: "pending", interval: 901 }, { status: "pending", reason: PRIVATE },
  { status: "authorized", models: null }, { status: "authorized", models: [{}] },
  { status: "error", error: PRIVATE }, { access_token: PRIVATE },
  { status: "authorized", models: [], error: PRIVATE },
]) {
  test("malformed poll input is terminal and redacted", () => {
    const output = login.parseCopilotPoll(input);
    assert.equal(output.status, "error");
    assert.equal(JSON.stringify(output).includes(PRIVATE), false);
  });
}

function moduleFrom(path, dependencies, globals = {}) {
  const source = readFileSync(new URL(path, import.meta.url), "utf8");
  const compiled = ts.transpileModule(source, { compilerOptions: {
    module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX,
  } }).outputText;
  const exports = {};
  runInNewContext(compiled, {
    exports, Headers, Response, AbortSignal, fetch,
    require: name => {
      if (Object.hasOwn(dependencies, name)) return dependencies[name];
      throw new Error(`Unexpected test dependency: ${name}`);
    }, ...globals,
  });
  return exports;
}

async function server(t, handler) {
  const calls = [];
  const http = createServer(async (req, res) => {
    const chunks = [];
    for await (const chunk of req) chunks.push(chunk);
    const raw = Buffer.concat(chunks).toString();
    const input = raw ? JSON.parse(raw) : undefined;
    calls.push({ path: req.url, input, principal: Boolean(req.headers["x-kars-principal-token"]) });
    const answer = await handler(req.url, input);
    res.writeHead(answer.status ?? 200, { "content-type": "application/json", ...answer.headers });
    res.end(JSON.stringify(answer.body));
  });
  await new Promise(resolve => http.listen(0, "127.0.0.1", resolve));
  t.after(async () => { http.closeAllConnections(); await new Promise(resolve => http.close(resolve)); });
  return { origin: `http://127.0.0.1:${http.address().port}`, calls };
}

function client(origin) {
  const bff = moduleFrom("../src/lib/bff.ts", {
    "./config": { bffBaseUrl: () => origin },
    "next/headers": { cookies: async () => ({ get: () => ({ value: "signed-test-session" }) }) },
    "./session-token": { SESSION_COOKIE: "bridge-session", verifySession: async () => true },
    "./oidc-config": { ssoConfigured: () => true },
    "./credential-review": {},
  });
  const invalidations = [];
  const actions = moduleFrom("../src/app/console/configuration/copilot-login-actions.ts", {
    "@/lib/bff": bff, "@/lib/copilot-login": login,
    "next/cache": { revalidatePath: path => invalidations.push(path) },
  });
  return { actions, invalidations };
}

async function until(condition) {
  const deadline = Date.now() + 3000;
  while (!condition()) {
    if (Date.now() > deadline) throw new Error("Timed out waiting for local test HTTP");
    await new Promise(resolve => setTimeout(resolve, 1));
  }
}

test("BFF producer fixtures -> real HTTP client -> actions -> controller preserve cumulative intervals", async t => {
  let polls = 0;
  const remote = await server(t, path => ({
    body: path.endsWith("/start") ? contract.start : [contract.slow_down_1, contract.slow_down_2, contract.authorized][polls++],
  }));
  const { actions, invalidations } = client(remote.origin);
  const h = controller(actions.copilotLoginPollAction, { start: actions.copilotLoginStartAction });
  await h.control.begin();
  await h.time.advance(5000);
  await until(() => h.time.deadlines().includes(15000));
  await h.time.advance(10_000);
  await until(() => h.time.deadlines().includes(30000));
  await h.time.advance(15_000);
  await until(() => h.view.state.authorized.length === 1);
  assert.deepEqual(remote.calls.filter(c => c.path.endsWith("/poll")).map(c => c.input.interval), [5, 10, 15]);
  assert.ok(remote.calls.every(c => c.principal && !c.path.includes(contract.start.device_code)));
  assert.equal(h.view.state.flow, null);
  assert.deepEqual(invalidations, ["/console/configuration"]);
  assert.deepEqual(h.time.deadlines(), []);
});

test("real HTTP polling remains one request while the upstream response is held", async t => {
  const held = deferred();
  const remote = await server(t, path => path.endsWith("/start") ? { body: contract.start } : held.promise);
  const { actions } = client(remote.origin);
  const h = controller(actions.copilotLoginPollAction, { start: actions.copilotLoginStartAction });
  await h.control.begin();
  await h.time.advance(5000);
  await until(() => remote.calls.length === 2);
  await h.time.advance(25_000);
  assert.equal(remote.calls.length, 2);
  h.control.cancel();
  held.resolve({ body: contract.authorized });
  await flush();
  assert.equal(h.view.state.authorized.length, 0);
});

test("non-2xx, unknown errors, invalid JSON shapes and persistence failures cannot become pending or authorized", async t => {
  let answer = { body: {} };
  const remote = await server(t, () => answer);
  const { actions, invalidations } = client(remote.origin);
  for (const body of [{}, { access_token: PRIVATE }, { error: { code: PRIVATE, message: PRIVATE } }]) {
    answer = { body };
    const result = await actions.copilotLoginPollAction(contract.start.device_code, 5);
    assert.equal(result.status, "error");
    assert.equal(JSON.stringify(result).includes(PRIVATE), false);
  }
  for (const status of [302, 400, 401, 403, 429, 500, 502]) {
    answer = { status, body: { error: { code: PRIVATE, message: PRIVATE } } };
    const result = await actions.copilotLoginPollAction(contract.start.device_code);
    assert.equal(result.status, "error");
    assert.equal(JSON.stringify(result).includes(PRIVATE), false);
  }
  for (const key of ["not_ready", "storage_unconfirmed"]) {
    answer = { status: 409, body: contract[key] };
    const result = await actions.copilotLoginPollAction(contract.start.device_code);
    assert.equal(result.status, "error");
    assert.equal(result.error, login.copilotErrors[contract[key].error.code]);
  }
  answer = { status: 409, body: contract.not_ready };
  assert.equal((await actions.copilotLoginStartAction()).ok, false);
  assert.deepEqual(invalidations, []);
});

test("a start-time grant refusal prevents the UI from scheduling any token polling", async t => {
  const remote = await server(t, () => ({ status: 409, body: contract.not_ready }));
  const { actions } = client(remote.origin);
  const h = controller(actions.copilotLoginPollAction, { start: actions.copilotLoginStartAction });
  await h.control.begin();
  await h.time.advance(900_000);
  assert.equal(remote.calls.length, 1);
  assert.ok(remote.calls[0].path.endsWith("/start"));
  assert.equal(h.view.state.flow, null);
  assert.equal(h.view.state.error, login.copilotErrors.copilot_not_ready);
});

test("redirect responses cannot forward a device POST or principal to another endpoint", async t => {
  const remote = await server(t, () => ({
    status: 307, headers: { location: "/unexpected" }, body: contract.authorized,
  }));
  const { actions, invalidations } = client(remote.origin);
  assert.equal((await actions.copilotLoginStartAction()).ok, false);
  assert.equal((await actions.copilotLoginPollAction(contract.start.device_code, 5)).status, "error");
  assert.equal(remote.calls.length, 2);
  assert.ok(remote.calls.every(call => call.path.startsWith("/api/operator/providers/copilot/login/")));
  assert.deepEqual(invalidations, []);
});

test("authorized model projection retains the DTO but never extra credential fields", () => {
  const result = login.parseCopilotPoll({ status: "authorized", access_token: PRIVATE,
    models: [{ id: "model-1", label: null, recommended: true, detail: "live detail", token: PRIVATE }] });
  assert.equal(result.status, "authorized");
  assert.equal(result.models[0].detail, "live detail");
  assert.equal(JSON.stringify(result).includes(PRIVATE), false);
});

test("the actual sign-in component clears its waiting UI after an action transport rejection", async () => {
  const time = clock();
  const hooks = [], effects = [];
  let cursor = 0, mounted = true;
  const react = {
    useState(initial) {
      const index = cursor++;
      if (!(index in hooks)) hooks[index] = initial;
      return [hooks[index], value => { assert.ok(mounted); hooks[index] = value; }];
    },
    useRef(initial) {
      const index = cursor++;
      if (!(index in hooks)) hooks[index] = { current: initial };
      return hooks[index];
    },
    useEffect(callback, dependencies) {
      const index = cursor++;
      const previous = hooks[index];
      if (!previous || dependencies.some((value, n) => !Object.is(value, previous.dependencies[n]))) {
        effects.push(() => { previous?.cleanup?.(); hooks[index] = { dependencies, cleanup: callback() }; });
      }
    },
  };
  const jsx = (type, props) => ({ type, props });
  const { CopilotSignIn } = moduleFrom("../src/app/console/configuration/copilot-sign-in.tsx", {
    react, "react/jsx-runtime": { jsx, jsxs: jsx }, "@/components/icon": { Icon: () => null },
    "@/lib/copilot-login": { createCopilotLoginController: (service, ui) => login.createCopilotLoginController(service, ui, time) },
    "./copilot-login-actions": {
      copilotLoginStartAction: async () => ({ ok: true, data: contract.start }),
      copilotLoginPollAction: async () => { throw new Error(PRIVATE); },
    },
  });
  const props = { signedIn: false, onAuthorized: () => assert.fail("must not authorize") };
  function render() {
    cursor = 0;
    const tree = CopilotSignIn(props);
    effects.splice(0).forEach(effect => effect());
    return tree;
  }
  function nodes(tree) {
    if (Array.isArray(tree)) return tree.flatMap(nodes);
    if (!tree || typeof tree !== "object") return [];
    return [tree, ...nodes(tree.props?.children)];
  }
  const button = nodes(render()).find(node => node.type === "button");
  button.props.onClick();
  await flush();
  assert.ok(JSON.stringify(render()).includes("Checking sign-in status"));
  await time.advance(5000);
  const output = JSON.stringify(render());
  assert.ok(output.includes(login.copilotErrors.transport));
  assert.equal(output.includes("Waiting for approval"), false);
  assert.equal(output.includes("Checking sign-in status"), false);
  assert.equal(output.includes(PRIVATE), false);
  hooks.forEach(hook => hook?.cleanup?.());
  mounted = false;
  await time.advance(900_000);
});
