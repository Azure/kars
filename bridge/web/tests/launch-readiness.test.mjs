// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const ts = createRequire(import.meta.url)("typescript");
const jsx = (type, props) => typeof type === "function" ? type(props) : { type, props };
const jsxRuntime = { jsx, jsxs: jsx, Fragment: "fragment" };
function load(path, imports = {}, globals = {}) {
  const context = { exports: {}, ...globals, require(name) {
    if (name === "react/jsx-runtime") return jsxRuntime;
    assert.ok(name in imports, `Unexpected import: ${name}`);
    return imports[name];
  } };
  runInNewContext(ts.transpileModule(readFileSync(new URL(`../src/${path}`, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022,
      jsx: ts.JsxEmit.ReactJSX },
  }).outputText, context);
  return context.exports;
}
const result = (status = "pass", ok = status !== "fail") => ({ ok,
  checks: [{ id: "budget", label: "Budget", status, detail: "Budget check detail" }] });
const tick = () => new Promise(resolve => setImmediate(resolve));
function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function actions(fetch) {
  return load("app/tasks/[name]/launch-actions.ts", {
    "@/lib/config": { defaultNamespace: () => "workspace/a" },
    "@/lib/bff": { authenticatedBffFetch: fetch },
  });
}

test("stored validation uses the authenticated, encoded, uncached POST and preserves warnings", async () => {
  for (const status of ["pass", "warn", "fail"]) {
    let request;
    const api = actions(async (...args) => {
      request = args;
      return { ok: true, json: async () => result(status) };
    });
    const actual = await api.validateTask("mission/a");
    assert.equal(actual.ok, status !== "fail");
    assert.equal(actual.checks[0].status, status);
    assert.equal(request[0], "/api/namespaces/workspace%2Fa/tasks/mission%2Fa/validate");
    assert.equal(request[1].method, "POST");
    assert.equal(request[1].cache, "no-store");
  }
});

test("transport, HTTP, and unreadable responses fail closed without exposing upstream secrets", async () => {
  for (const fetch of [
    async () => { throw new Error("private credential"); },
    async () => ({ ok: false, status: 403 }),
    async () => ({ ok: true, json: async () => { throw new Error("private payload"); } }),
  ]) {
    const actual = await actions(fetch).validateTask("draft");
    assert.equal(actual.ok, false);
    assert.equal(actual.checks[0].status, "fail");
    assert.doesNotMatch(JSON.stringify(actual), /private/);
  }
});

test("malformed, empty, coerced-status and inconsistent successful responses cannot enable launch", async () => {
  for (const payload of [null, [], {}, { ok: true, checks: [] },
    { ok: "true", checks: result().checks }, { ok: true, checks: [null] },
    { ok: true, checks: [{ ...result().checks[0], status: ["pass"] }] },
    { ok: true, checks: [{ ...result().checks[0], detail: null }] },
    result("fail", true), result("pass", false),
  ]) {
    const actual = await actions(async () => ({ ok: true, json: async () => payload })).validateTask("draft");
    assert.equal(actual.ok, false, JSON.stringify(payload));
  }
});

const statusModule = load("components/mission-status.tsx");
const draft = { name: "draft", namespace: "workspace", created_at: "2026-10-02T00:00:00Z",
  phase: "Ready", launched: false, envelope: { budget: { tokens: 20000 } },
  envelope_digest: "digest", observed_generation: 1, result: null, activity: [],
  assignment: null, current_run_nonce: null, execution_phase: null };
function text(node) {
  if (Array.isArray(node)) return node.map(text).join("");
  if (node && typeof node === "object") return text(node.props?.children);
  return node == null || typeof node === "boolean" ? "" : String(node);
}
function nodes(node, predicate) {
  if (Array.isArray(node)) return node.flatMap(child => nodes(child, predicate));
  if (!node || typeof node !== "object") return [];
  return [...(predicate(node) ? [node] : []), ...nodes(node.props?.children, predicate)];
}

// Run the production component with controllable hook commits and network replies.
function panel(initial = draft) {
  let task = initial, cursor = 0, tree, pending = false;
  const slots = [], effects = [], requests = [], launches = [], runs = [], transitions = [];
  const router = { refresh() {} };
  const react = {
    useState(initialValue) {
      const index = cursor++;
      if (!(index in slots)) slots[index] = initialValue;
      return [slots[index], value => { slots[index] = typeof value === "function" ? value(slots[index]) : value; }];
    },
    useRef(value) { const index = cursor++; return slots[index] ??= { current: value }; },
    useEffect(effect, deps) {
      const index = cursor++, old = slots[index];
      if (!old || deps.some((value, i) => !Object.is(value, old.deps[i]))) {
        effects.push(() => { old?.cleanup?.(); slots[index] = { deps, cleanup: effect() }; });
      }
    },
    useTransition() { return [pending, fn => {
      pending = true;
      transitions.push(Promise.resolve(fn()).finally(() => { pending = false; }));
    }]; },
  };
  const { ExecutionPanel } = load("app/tasks/[name]/execution-panel.tsx", {
    react, "next/navigation": { useRouter: () => router },
    "@/components/status-badge": { StatusBadge: ({ label }) => jsx("span", { children: label }) },
    "@/components/mission-status": statusModule,
    "@/lib/run-mission-client": { runMissionClient: async (...args) => { runs.push(args); return { error: null }; } },
    "./launch-actions": {
      validateTask: name => { const request = { name, ...deferred() }; requests.push(request); return request.promise; },
      setLaunch: async (...args) => { launches.push(args); return { error: null }; },
    },
  }, { window: { setTimeout() {}, setInterval() { return 1; }, clearInterval() {} } });
  const harness = {
    requests, launches, runs,
    render(next = task) {
      task = next; cursor = 0; tree = ExecutionPanel({ task });
      while (effects.length) effects.shift()();
      return tree;
    },
    button(label) { return nodes(tree, n => n.type === "button" && text(n) === label)[0]?.props; },
    text: () => text(tree),
    async settle(index, value = result()) { requests[index].resolve(value); await tick(); harness.render(); },
    async finish() { await Promise.all(transitions); harness.render(); },
    unmount() { for (const slot of slots) slot?.cleanup?.(); },
  };
  harness.render();
  return harness;
}

test("a Ready draft starts checking, blocks failed validation, and explicitly rechecks", async () => {
  const ui = panel();
  assert.equal(ui.requests.length, 1);
  assert.equal(ui.button("Launch").disabled, true);
  assert.match(ui.text(), /Checking this saved package/);
  assert.doesNotMatch(ui.text(), /Ready to launch/);
  await ui.settle(0, result("fail"));
  assert.equal(ui.button("Launch").disabled, true);
  assert.match(ui.text(), /Launch blocked/);
  ui.button("Recheck launch").onClick(); ui.render();
  assert.equal(ui.button("Launch").disabled, true);
  await ui.settle(1, result("warn"));
  assert.equal(ui.button("Launch").disabled, false);
  assert.match(ui.text(), /completed with warnings/);
});

test("admission must also be Ready and unrelated activity does not retrigger validation", async () => {
  const ui = panel({ ...draft, phase: "Pending" });
  await ui.settle(0);
  assert.equal(ui.button("Launch").disabled, true);
  ui.render({ ...draft, phase: "Pending", activity: [{ message: "updated" }] });
  assert.equal(ui.requests.length, 1);
  ui.render(draft);
  assert.equal(ui.requests.length, 2);
  assert.equal(ui.button("Launch").disabled, true);
  await ui.settle(1);
  assert.equal(ui.button("Launch").disabled, false);
});

test("replaced package validation cannot enable Launch with a stale success", async () => {
  const ui = panel();
  ui.render({ ...draft, observed_generation: 2, envelope_digest: "changed" });
  await ui.settle(0);
  assert.equal(ui.button("Launch").disabled, true);
  await ui.settle(1, result("fail"));
  assert.equal(ui.button("Launch").disabled, true);
});

test("Launch revalidates and does not mutate when the new validation fails", async () => {
  const ui = panel();
  await ui.settle(0);
  ui.button("Launch").onClick(); ui.render();
  assert.equal(ui.launches.length, 0);
  await ui.settle(1, result("fail")); await ui.finish();
  assert.equal(ui.launches.length, 0);
  assert.equal(ui.button("Launch").disabled, true);
});

test("validated Launch uses the original action only after the fresh check passes", async () => {
  const ui = panel();
  await ui.settle(0);
  ui.button("Launch").onClick();
  await ui.settle(1); await ui.finish();
  assert.deepEqual(ui.launches, [["draft", true]]);
  assert.ok(ui.button("Stop"));
});

test("package change while action validation is in flight discards the launch attempt", async () => {
  const ui = panel();
  await ui.settle(0);
  ui.button("Launch").onClick();
  ui.render({ ...draft, envelope_digest: "new" });
  await ui.settle(1); await ui.finish();
  assert.equal(ui.launches.length, 0);
  assert.equal(ui.button("Launch").disabled, true);
  await ui.settle(2);
  assert.equal(ui.button("Launch").disabled, false);
});

test("initial and action transport failures are recoverable and keep Launch disabled", async () => {
  const ui = panel();
  ui.requests[0].reject(new Error("offline")); await tick(); ui.render();
  assert.equal(ui.button("Launch").disabled, true);
  assert.equal(ui.button("Recheck launch").disabled, false);
  ui.button("Recheck launch").onClick(); ui.render(); await ui.settle(1);
  ui.button("Launch").onClick(); ui.requests[2].reject(new Error("action offline")); await ui.finish();
  assert.equal(ui.launches.length, 0);
  assert.equal(ui.button("Launch").disabled, true);
  assert.equal(ui.button("Recheck launch").disabled, false);
});

test("Stop and delivered reruns remain independent of draft validation", async () => {
  const running = panel({ ...draft, launched: true, execution_phase: "Running" });
  assert.equal(running.requests.length, 0);
  running.button("Stop").onClick(); await running.finish();
  assert.deepEqual(running.launches, [["draft", false]]);
  const delivered = panel({ ...draft, launched: true, execution_phase: "Running",
    result: { status: "success", finished_at: "now", output: "Useful work" } });
  assert.match(delivered.text(), /Delivered/);
  assert.equal(delivered.requests.length, 0);
  delivered.button("Run again").onClick(); await delivered.finish();
  assert.deepEqual(delivered.runs, [["workspace", "draft"]]);
  assert.match(delivered.text(), /corrected rerun is in progress/);
});

test("draft naming does not override blocked, failed, delivered or active execution precedence", () => {
  assert.equal(text(statusModule.MissionStatusBadge({ status: "drafting" })), "Draft");
  assert.equal(statusModule.missionStatus("Ready", "Running", { failed: true }), "failed");
  assert.equal(statusModule.missionStatus("Degraded", "Running", { delivered: true }), "blocked");
  assert.equal(statusModule.missionStatus("Ready", "Running", { delivered: true }), "done");
  assert.equal(statusModule.missionStatus("Ready", "Running"), "running");
});
