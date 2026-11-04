// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const code = ts.transpileModule(readFileSync(new URL("../src/app/workspace/teams/[name]/team-edit.tsx", import.meta.url), "utf8"), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
}).outputText;
function elements(node) {
  if (Array.isArray(node)) return node.flatMap(elements);
  if (!node || typeof node !== "object" || !node.props) return [];
  return [node, ...elements(node.props.children)];
}
function harness(status = 200) {
  const states = [], calls = [], transitions = [], exports = {};
  let cursor = 0, refreshes = 0;
  const phase = name => ({ name, objective: "Research and retain findings", capabilities: ["network", "filesystem-write"], max_tool_calls: 0, fresh_context: true, execution_mode: "native" });
  const props = { ns: "work", name: "eng", charter: "Original charter", paused: true, everyMinutes: null,
    mcpServers: ["approved-mcp"], availableMcp: [], egress: ["github.com:443"], model: null, models: [],
    modelFallbacks: [], memory: null, availableMemories: [], gitWriteRepos: [],
    executionPlan: { schema: "kars.execution-plan/v1", max_parallel: 2,
      roles: ["benchmark-research", "control-research"].map(name => ({ name, objective: "Research", depends_on: [], phases: [phase("research")] })),
      synthesis: { objective: "Package", capabilities: ["filesystem-read", "filesystem-write"], max_tool_calls: 12, execution_mode: "native" },
      deliverables: [{ name: "package.md" }] },
  };
  runInNewContext(code, { exports, structuredClone, fetch: async (...args) => {
    calls.push(args);
    return { status, ok: status === 200, json: async () => ({ error: { message: "ReviewedInputCapabilityRequired: benchmark-research needs a positive allowance" } }) };
  }, require: name => {
    if (name === "react") return {
      useState(initial) { const index = cursor++; if (!(index in states)) states[index] = typeof initial === "function" ? initial() : initial;
        return [states[index], value => { states[index] = typeof value === "function" ? value(states[index]) : value; }]; },
      useTransition: () => [false, callback => transitions.push(callback())],
    };
    if (name === "next/navigation") return { useRouter: () => ({ refresh: () => refreshes++ }) };
    if (name === "@/components/repo-access") return { RepoAccess: () => null };
    if (name === "@/lib/auth-return") return { loginPath: () => "/login" };
    return require(name);
  } });
  return { props, calls, render() { cursor = 0; return elements(exports.TeamEdit(props)); },
    async settle() { await Promise.all(transitions); }, get refreshes() { return refreshes; } };
}
const button = (h, label) => h.render().find(node => node.type === "button" && node.props.children === label);
const input = (h, label) => h.render().find(node => node.type === "input" && node.props["aria-label"] === label);
const open = h => button(h, "Edit").props.onClick();

test("retained-input remedy is explicit, role-scoped, and saves only the plan", async () => {
  const h = harness();
  const original = structuredClone(h.props.executionPlan);
  open(h);
  assert.equal(h.calls.length, 0);
  assert.ok(h.render().some(node => node.type === "legend" && node.props.children === "Reading previous work"));
  assert.ok(h.render().some(node => typeof node.props.children === "string" && node.props.children.includes("these controls cannot override that policy")));
  const read = "Read previous work for benchmark-research / research";
  assert.equal(input(h, read).props.checked, false);
  input(h, read).props.onChange({ target: { checked: true } });
  assert.equal(input(h, "Maximum tool calls for benchmark-research / research").props.value, 0, "read opt-in must not silently grant tool calls");
  input(h, "Maximum tool calls for benchmark-research / research").props.onChange({ target: { value: "8" } });
  assert.equal(input(h, "Read previous work for control-research / research").props.checked, false);
  h.render().find(node => node.type === "textarea" && node.props.value === "Original charter").props.onChange({ target: { value: "Unsaved charter" } });
  button(h, "Save input permissions").props.onClick();
  await h.settle();
  const expected = structuredClone(original);
  expected.roles[0].phases[0].capabilities.push("filesystem-read");
  expected.roles[0].phases[0].max_tool_calls = 8;
  assert.deepEqual(JSON.parse(h.calls[0][1].body), { execution_plan: expected });
  assert.equal(h.calls[0][1].method, "PATCH");
  assert.equal(h.calls[0][0], "/api/namespaces/work/teams/eng");
  assert.deepEqual(h.props.executionPlan, original);
  assert.equal(h.refreshes, 1);
});

test("permission controls require a paused Team and do not auto-grant on resume", async () => {
  const h = harness();
  h.props.paused = false;
  open(h);
  assert.equal(button(h, "Save input permissions").props.disabled, true);
  assert.equal(input(h, "Read previous work for benchmark-research / research").props.disabled, true);
  assert.equal(h.calls.length, 0);
  h.props.paused = true;
  input(h, "Read previous work for benchmark-research / research").props.onChange({ target: { checked: true } });
  button(h, "Resume").props.onClick();
  await h.settle();
  assert.deepEqual(JSON.parse(h.calls[0][1].body), { paused: false });
});

test("failed save preserves explicit draft and feedback without widening a zero allowance", async () => {
  const h = harness(400);
  open(h);
  const read = "Read previous work for benchmark-research / research";
  input(h, read).props.onChange({ target: { checked: true } });
  button(h, "Save input permissions").props.onClick();
  await h.settle();
  assert.equal(input(h, read).props.checked, true);
  assert.equal(input(h, "Maximum tool calls for benchmark-research / research").props.value, 0);
  assert.ok(h.render().some(node => typeof node.props.children === "string" && node.props.children.includes("needs a positive allowance")));
  assert.equal(h.refreshes, 0);
  assert.equal(JSON.parse(h.calls[0][1].body).execution_plan.roles[0].phases[0].max_tool_calls, 0);
});

for (const [kind, message, retained] of [
  ["retained-input admission", "This run cannot start because some phases cannot read previous work: benchmark-research. Pause the Team, open Edit → Reading previous work. [ReviewedInputCapabilityRequired]", true],
  ["legacy retained-input diagnostic", "ReviewedInputCapabilityRequired: benchmark-research needs permission to read previous work.", true],
  ["administrator policy denial", "An administrator's tool policy denied reading previous work. Ask your administrator to review this policy.", false],
]) {
  test(`Run now displays ${kind} plainly without claiming a run was requested`, async () => {
    const source = readFileSync(new URL("../src/app/workspace/teams/[name]/run-control.tsx", import.meta.url), "utf8");
    const exports = {}, states = [], transitions = [];
    let cursor = 0, calls = 0, refreshes = 0;
    runInNewContext(ts.transpileModule(source, { compilerOptions: {
      module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX,
    } }).outputText, { exports, require: name => {
      if (name === "react") return {
        useState(initial) { const index = cursor++; if (!(index in states)) states[index] = initial;
          return [states[index], value => { states[index] = value; }]; },
        useTransition: () => [false, callback => transitions.push(callback())],
      };
      if (name === "next/navigation") return { useRouter: () => ({ refresh: () => refreshes++ }) };
      if (name === "@/components/live-refresh") return { LiveRefresh: () => null };
      if (name === "./run-actions") return { runTeamNow: async () => { calls++; return { error: message }; } };
      return require(name);
    } });
    const h = { render() { cursor = 0; return elements(exports.RunNowControl({ team: "eng", paused: false, inFlight: false, runKey: "old-run" })); } };
    button(h, "Run now").props.onClick();
    assert.equal(calls, 0);
    button(h, "Confirm run").props.onClick();
    await Promise.all(transitions);
    assert.equal(calls, 1);
    assert.equal(refreshes, 0);
    const alert = h.render().find(node => node.props.role === "alert");
    assert.ok(alert);
    const primary = alert.props.children[0].props.children;
    assert.doesNotMatch(primary, /ReviewedInputCapabilityRequired/);
    if (retained) {
      assert.match(primary, /^This run cannot start/);
      assert.match(primary, /benchmark-research/);
      assert.ok(elements(alert).some(node => node.type === "details"));
      assert.ok(elements(alert).some(node => node.props.children === "Technical details"));
      assert.ok(elements(alert).some(node => node.type === "p" && node.props.children === message));
    } else {
      assert.equal(primary, message);
      assert.doesNotMatch(primary, /Save input permissions|enable|Pause/);
      assert.equal(elements(alert).some(node => node.type === "details"), false);
    }
    assert.ok(button(h, "Run now"));
    assert.equal(button(h, "Confirm run"), undefined);
  });
}

test("malformed advanced plan drafts cannot crash the structured permission editor", () => {
  for (const value of ["{", "null", '{"roles":[null]}', '{"roles":[{"phases":[null]}]}']) {
    const h = harness();
    open(h);
    h.render().find(node => node.type === "textarea" && node.props.value.includes('"kars.execution-plan/v1"')).props.onChange({ target: { value } });
    assert.equal(button(h, "Save input permissions"), undefined);
    assert.equal(h.calls.length, 0);
  }
});
