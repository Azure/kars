// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const React = require("react");
const { renderToStaticMarkup } = require("react-dom/server");
const source = readFileSync(new URL("../src/components/fleet-live.tsx", import.meta.url), "utf8");
function load(react = React, globals = {}) {
  const imports = {
    react,
    "react/jsx-runtime": require("react/jsx-runtime"),
    "@/components/navigation-link": { default: ({ children, ...props }) => React.createElement("a", props, children) },
  };
  const context = { exports: {}, ...globals, require(name) {
    assert.ok(Object.hasOwn(imports, name), `unexpected dependency ${name}`);
    return imports[name];
  } };
  runInNewContext(ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, context);
  return context.exports.FleetLive;
}
const FleetLive = load();
const empty = { working: 0, teams_active: 0, sub_agents: 0, tokens_in_flight: null, tool_calls: null, rounds: null, activity_unavailable: 0, feed: [] };
const render = (initial) => renderToStaticMarkup(React.createElement(FleetLive, { initial }));

test("unavailable activity never becomes zero, calm, or warming up", () => {
  const html = render(null);
  assert.match(html, /Live activity is unavailable/);
  assert.match(html, /does not mean your work has stopped/);
  assert.doesNotMatch(html, /The fleet is calm|warming up|>0<|streaming/);
});

test("no worker does not imply a queued run stopped", () => {
  const html = render(empty);
  assert.match(html, /No active worker found/);
  assert.match(html, /queued run may still be waiting/);
  assert.doesNotMatch(html, /every tool call|The fleet is calm/);
});

test("active worker without evidence stays unavailable, not zero or warmup", () => {
  const html = render({ ...empty, working: 1, teams_active: 1, sub_agents: 1, activity_unavailable: 1 });
  assert.match(html, /1 active worker; no detailed observations available yet/);
  assert.match(html, /Activity is unavailable for 1 active worker/);
  assert.match(html, /not complete run totals or settled spending/);
  assert.doesNotMatch(html, /warming up/);
});

test("real observed zeros remain distinguishable from missing evidence", () => {
  const html = render({ ...empty, working: 1, rounds: 0, tool_calls: 0 });
  assert.match(html, /Observed tool results/);
  assert.match(html, /Observed model responses/);
  assert.equal((html.match(/>0<\/p>/g) ?? []).length, 4);
});

test("reviewed results link to their run and retain source, role and failure semantics", () => {
  const html = render({ ...empty, working: 1, rounds: 2, tool_calls: 1, feed: [{
    agent: "run-1", worker: "worker-1", role: "qa / test", display_name: "Monitor", team: "team-1",
    kind: "tool", label: "python_execute", detail: null, failed: true, round: 2, seq: 4,
    ms: 4, source: "harness", observed_at: null,
  }] });
  assert.match(html, /href="\/workspace\/teams\/team-1\/runs\/run-1"/);
  assert.match(html, /reported result/);
  assert.match(html, /qa \/ test · harness · timestamp unavailable/);
  assert.match(html, /failed/);
  assert.doesNotMatch(html, /called /);
});

test("failed refresh removes stale evidence and waits before the next request", async () => {
  const effects = [], states = [], timers = [];
  let requests = 0;
  const Component = load({ ...React,
    useState: (value) => [value, (next) => states.push(next)],
    useRef: (value) => ({ current: value }),
    useEffect: (effect) => effects.push(effect),
  }, {
    AbortController,
    fetch: async () => { requests++; return { ok: false }; },
    setTimeout: (fn, ms) => { timers.push({ fn, ms }); return 1; },
    clearTimeout: () => {},
  });
  Component({ initial: { ...empty, working: 1 } });
  const cleanup = effects[0]();
  await new Promise(setImmediate);
  assert.equal(requests, 1);
  assert.equal(states.at(-1), null);
  assert.equal(timers.length, 2);
  assert.equal(timers[0].ms, 8000);
  assert.equal(timers[1].ms, 3000);
  cleanup();
});

for (const unmount of [false, true]) {
  test(unmount ? "unmount aborts pending activity without updates or retry" : "timed-out activity clears stale evidence and recovers on the next poll", async () => {
    const effects = [], states = [], timers = new Map();
    let requests = 0, timerId = 0, signal;
    const recovered = { ...empty, working: 2 };
    const Component = load({ ...React,
      useState: (value) => [value, (next) => states.push(next)],
      useRef: (value) => ({ current: value }),
      useEffect: (effect) => effects.push(effect),
    }, {
      AbortController,
      fetch: async (_url, options) => {
        requests++;
        if (requests > 1) return { ok: true, json: async () => recovered };
        signal = options.signal;
        return new Promise((_resolve, reject) => signal.addEventListener("abort", () => reject(new Error("Aborted")), { once: true }));
      },
      setTimeout: (fn, ms) => { timers.set(++timerId, { fn, ms }); return timerId; },
      clearTimeout: (id) => timers.delete(id),
    });
    Component({ initial: { ...empty, working: 1 } });
    const cleanup = effects[0]();
    assert.equal(requests, 1);
    if (unmount) cleanup();
    else {
      const timeout = timers.get(1);
      assert.equal(timeout.ms, 8000);
      timeout.fn();
    }
    await new Promise(setImmediate);
    assert.equal(signal.aborted, true);
    if (unmount) {
      assert.equal(states.length, 0);
      assert.equal(timers.size, 0);
    } else {
      assert.equal(states.at(-1), null);
      assert.equal(timers.size, 1);
      const [id, retry] = [...timers][0];
      assert.equal(retry.ms, 3000);
      timers.delete(id);
      retry.fn();
      await new Promise(setImmediate);
      assert.equal(requests, 2);
      assert.equal(states.at(-1), recovered);
      cleanup();
      assert.equal(timers.size, 0);
    }
  });
}
