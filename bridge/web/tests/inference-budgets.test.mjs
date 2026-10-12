// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import { test } from "node:test";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const source = readFileSync(new URL("../src/components/inference-budgets.tsx", import.meta.url), "utf8");

function fixture(fetch, initial = []) {
  const states = [...initial];
  let cursor = 0;
  const react = {
    useState(value) {
      const index = cursor++;
      if (!(index in states)) states[index] = value;
      return [states[index], (next) => { states[index] = typeof next === "function" ? next(states[index]) : next; }];
    },
    useCallback: (fn) => fn,
    useId: () => "budget-suggestions",
    useEffect() {},
  };
  const exports = {};
  runInNewContext(ts.transpileModule(source + "\nexport { BudgetCard, AddWorkspace };", {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, {
    exports, fetch, AbortSignal, Error,
    require(name) {
      if (name === "react") return react;
      if (name === "@/components/icon") return { Icon: () => null };
      return require(name);
    },
  });
  return { ...exports, states, render(name, props) { cursor = 0; return exports[name](props); } };
}

function nodes(node) {
  if (Array.isArray(node)) return node.flatMap(nodes);
  if (!node || typeof node !== "object") return [];
  return [node, ...nodes(node.props?.children)];
}
const button = (tree, label) => nodes(tree).find(node => node.type === "button" && node.props.children === label);
const alert = (tree) => nodes(tree).find(node => node.props?.role === "alert");
const good = { daily_tokens: 1000, mode: "buffer", buffer_percent: 25 };

test("all budget writes validate numbers before sending and preserve request methods", async () => {
  const calls = [];
  const f = fixture(async (url, init) => { calls.push([url, init]); return { ok: true }; });
  for (const body of [
    { ...good, daily_tokens: -1 }, { ...good, daily_tokens: 1e30 },
    { ...good, daily_tokens: 1.5 }, { ...good, daily_tokens: NaN },
    { ...good, buffer_percent: -1 }, { ...good, buffer_percent: 1001 },
    { ...good, buffer_percent: 1.5 },
  ]) await assert.rejects(f.persistBudget("/budget", body), /must be/);
  assert.equal(calls.length, 0);
  await f.persistBudget("/budget", good);
  await f.persistBudget("/budget", { clear: true });
  await f.persistBudget("/policy", { daily_tokens: 17 }, "PATCH");
  assert.deepEqual(calls.map(([url, init]) => [url, init.method, JSON.parse(init.body)]), [
    ["/budget", "PUT", good], ["/budget", "PUT", { clear: true }],
    ["/policy", "PATCH", { daily_tokens: 17 }],
  ]);
  assert.ok(calls.every(([, init]) => init.signal instanceof AbortSignal));
});

test("HTTP denial, validation failure, upstream failure and transport loss never report success", async () => {
  for (const status of [400, 403, 409, 422, 500, 502]) {
    const f = fixture(async () => ({ ok: false, status }));
    await assert.rejects(f.persistBudget("/budget", good), status === 403 ? /Only a cluster or org admin/ : new RegExp(`HTTP ${status}`));
  }
  const f = fixture(async () => { throw new Error("PRIVATE_TRANSPORT_DETAIL"); });
  await assert.rejects(f.persistBudget("/budget", good), error =>
    error.message.includes("Check the current status") && !error.message.includes("PRIVATE"));
});

test("failed budget edits and removals keep entered values and re-enable controls", async () => {
  for (const operation of ["Save", "Remove cap"]) {
    const f = fixture(null, [true, 2000, "buffer", 25, false, null]);
    const reject = async () => { throw new Error("Budget update was not confirmed (HTTP 403)."); };
    const props = { title: "Workspace", subtitle: "", level: { daily_tokens: 1000 }, fallbackUsed: 0, onSave: reject, onRemove: reject };
    await button(f.render("BudgetCard", props), operation).props.onClick();
    const tree = f.render("BudgetCard", props);
    assert.match(alert(tree).props.children, /HTTP 403/);
    assert.equal(button(tree, "Save").props.disabled, false);
    assert.equal(f.states[0], true);
    assert.equal(f.states[1], 2000);
  }
});

test("failed workspace and user creation retain the form and recover from transport errors", async () => {
  for (const label of ["Workspace (namespace)", "User identity"]) {
    const f = fixture(null, ["test-scope", 1000, "passive", 20, false, null]);
    const props = { label, suggestions: [], defaultNs: "", onCancel() {}, onCreate: async () => { throw new Error("Couldn't confirm the new budget."); } };
    await button(f.render("AddWorkspace", props), "Add").props.onClick();
    const tree = f.render("AddWorkspace", props);
    assert.match(alert(tree).props.children, /Couldn't confirm/);
    assert.equal(button(tree, "Add").props.disabled, false);
    assert.equal(f.states[0], "test-scope");
  }
});

test("inline policy editor reports rejection, cancels safely and displays the saved amount", async () => {
  let reject = true;
  const f = fixture(async () => ({ ok: !reject, status: 403 }), [true, 75, false, null, null]);
  const props = { name: "policy", current: 10 };
  await button(f.render("InferenceBudgetEdit", props), "Save").props.onClick();
  let tree = f.render("InferenceBudgetEdit", props);
  assert.match(alert(tree).props.children, /Only a cluster/);
  assert.equal(f.states[0], true);
  assert.equal(f.states[3], null);
  reject = false;
  await button(tree, "Save").props.onClick();
  tree = f.render("InferenceBudgetEdit", props);
  assert.equal(f.states[0], false);
  assert.equal(f.states[3], 75);
  assert.deepEqual(Array.from(tree.props.children[0].props.children), ["75", " ✓"]);
  tree.props.onClick();
  tree = f.render("InferenceBudgetEdit", props);
  nodes(tree).find(node => node.type === "input").props.onChange({ target: { value: "90" } });
  nodes(tree).find(node => node.props?.["aria-label"] === "Cancel budget edit").props.onClick();
  tree = f.render("InferenceBudgetEdit", props);
  assert.deepEqual(Array.from(tree.props.children[0].props.children), ["75", " ✓"]);
});

test("every write uses checked transport; only the measured-usage GET remains raw fetch", () => {
  assert.equal((source.match(/await fetch\(/g) ?? []).length, 2);
  assert.equal((source.match(/await persistBudget\(/g) ?? []).length, 8);
});
