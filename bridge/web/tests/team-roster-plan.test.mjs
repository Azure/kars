// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const ts = createRequire(import.meta.url)("typescript");
const jsx = (type, props) => ({ type, props: props ?? {} });
const plain = value => JSON.parse(JSON.stringify(value));
function load(file, imports = {}, globals = {}) {
  const context = { exports: {}, Error, TextEncoder, ...globals, require(name) {
    if (name === "react/jsx-runtime") return { jsx, jsxs: jsx };
    assert.ok(name in imports, `Unexpected import ${name}`);
    return imports[name];
  } };
  runInNewContext(ts.transpileModule(readFileSync(new URL(`../src/${file}`, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, context);
  return context.exports;
}
const helper = load("lib/team-roster-plan.ts");
function nodes(tree) {
  if (Array.isArray(tree)) return tree.flatMap(nodes);
  if (!tree?.props) return [];
  return [tree, ...nodes(tree.props.children)];
}
function harness(status = 200) {
  const states = [], transitions = [], requests = [];
  let cursor = 0, refreshes = 0;
  const original = { schema: "kars.execution-plan/v1", max_parallel: 1,
    roles: [{ name: "builder", objective: "Build and report the application", depends_on: [], budget_tokens: 1234,
      phases: [{ name: "build", objective: "Build and report the application", capabilities: ["filesystem-read", "filesystem-write"],
        max_tool_calls: 55, fresh_context: true, execution_mode: "native" }] }],
    synthesis: { objective: "Package the application and findings", capabilities: ["filesystem-read", "filesystem-write"],
      max_tool_calls: 35, execution_mode: "native" }, deliverables: [{ name: "report.md" }] };
  const props = { ns: "work", name: "team", paused: true, executionPlan: structuredClone(original),
    roster: [{ name: "builder", system_prompt: "Build and report the application", model: "retired::model", skills: [], resources: undefined }],
    options: { models: [], runtimes: [], skills: [] }, resources: { memory: "private-store", mcp_servers: [], egress: [] } };
  const { TeamRosterEdit } = load("app/workspace/teams/[name]/team-roster-edit.tsx", {
    react: {
      useState(initial) { const index = cursor++; if (!(index in states)) states[index] = typeof initial === "function" ? initial() : initial;
        return [states[index], next => { states[index] = typeof next === "function" ? next(states[index]) : next; }]; },
      useTransition: () => [false, callback => transitions.push(callback())],
    }, "next/navigation": { useRouter: () => ({ refresh() { refreshes++; } }) },
    "@/lib/member-archetypes": { MEMBER_ARCHETYPES: [{ id: "qa", system_prompt: "Validate the delivered application", suggestedSkills: [] }] },
    "@/lib/team-roster-plan": helper,
    "@/components/viewport-portal": { ViewportPortal: "Portal" },
    "@/components/team-role-resources": { TeamRoleResourcePicker: "Resources" },
  }, { fetch: async (url, init) => { requests.push({ url, body: JSON.parse(init.body) });
    return { ok: status === 200, status, json: async () => ({ error: { message: "Existing policy denied this change" } }) }; } });
  const render = () => { cursor = 0; return nodes(TeamRosterEdit(props)); };
  const button = label => render().find(node => node.type === "button" && node.props.children === label);
  const input = label => render().find(node => node.props["aria-label"] === label);
  const open = () => button("Edit org").props.onClick();
  const add = () => { button("+ Add member").props.onClick(); input("New member name").props.onChange({ target: { value: "tester" } }); };
  return { props, original, requests, render, button, input, open, add,
    async save() { button("Save org").props.onClick(); await Promise.all(transitions); }, get refreshes() { return refreshes; } };
}

test("add member saves matching roster and plan, inherits commons input access, preserves existing authority", async () => {
  const h = harness(); h.open(); h.add();
  h.input("tester waits for builder").props.onChange({ target: { checked: true } });
  await h.save();
  assert.equal(h.requests.length, 1);
  const body = h.requests[0].body;
  assert.deepEqual(Object.keys(body).sort(), ["execution_plan", "roles"]);
  assert.deepEqual(body.roles.map(role => role.name), ["builder", "tester"]);
  assert.deepEqual(body.execution_plan.roles.map(role => role.name), ["builder", "tester"]);
  assert.deepEqual(body.execution_plan.roles[0], h.original.roles[0]);
  assert.deepEqual(body.execution_plan.synthesis, h.original.synthesis);
  assert.deepEqual(body.execution_plan.deliverables, h.original.deliverables);
  assert.equal(body.roles[0].model, "retired::model", "unavailable model must not silently become default");
  assert.equal("resources" in body.roles[0], false, "legacy resource omission stays omitted");
  assert.deepEqual(body.roles[1].resources, { mcp_servers: [], egress: [] }, "private store is not shared implicitly");
  const tester = body.execution_plan.roles[1];
  assert.deepEqual(tester.depends_on, ["builder"]);
  assert.deepEqual(tester.phases[0].capabilities, ["filesystem-read", "filesystem-write"]);
  assert.equal(tester.phases[0].max_tool_calls, 55);
  assert.equal(tester.phases[0].execution_mode, "native");
  assert.equal(tester.budget_tokens, undefined);
  assert.deepEqual(h.props.executionPlan, h.original);
  assert.equal(h.refreshes, 1);
});

test("Python execution is opt-in and requires explicit native read, write and network permissions", async () => {
  const h = harness(); h.open(); h.add();
  const label = "Run Python applications and tests (requires read, write and network permissions) for tester / contribute";
  assert.equal(h.input(label).props.checked, false);
  h.input(label).props.onChange({ target: { checked: true } });
  await h.save();
  assert.equal(h.requests.length, 0);
  assert.match(JSON.stringify(h.render()), /select each permission explicitly/);
  h.input("Use approved network destinations for tester / contribute").props.onChange({ target: { checked: true } });
  await h.save();
  assert.equal(h.requests.length, 1);
  assert.deepEqual(h.requests[0].body.execution_plan.roles[0], h.original.roles[0]);
  const tester = h.requests[0].body.execution_plan.roles[1];
  assert.deepEqual(tester.phases[0].capabilities, ["filesystem-read", "filesystem-write", "python-execute", "network"]);
  assert.equal(tester.budget_tokens, undefined);
  assert.equal(tester.phases[0].capabilities.includes("shell"), false);
  for (const capability of ["filesystem-read", "filesystem-write", "network"]) {
    const invalid = structuredClone(tester);
    invalid.phases[0].capabilities = invalid.phases[0].capabilities.filter(value => value !== capability);
    assert.throws(() => helper.rosterExecutionPlan(h.original, [invalid]), /Python execution needs/);
  }
  const legacy = structuredClone(tester); delete legacy.phases[0].execution_mode;
  assert.throws(() => helper.rosterExecutionPlan(h.original, [legacy]), /Python execution needs/);
});

test("rename rewires dependencies; removal requires an explicit work-order change", async () => {
  const h = harness(); h.open(); h.add();
  h.input("tester waits for builder").props.onChange({ target: { checked: true } });
  h.input("builder name").props.onChange({ target: { value: "engineer" } });
  assert.equal(h.input("tester waits for engineer").props.checked, true);
  const remove = () => h.render().find(node => node.type === "button" && node.props.title === "Remove member").props.onClick();
  remove();
  assert.ok(h.input("engineer name"));
  assert.match(JSON.stringify(h.render()), /First update/);
  h.input("tester waits for engineer").props.onChange({ target: { checked: false } });
  remove(); await h.save();
  assert.deepEqual(h.requests[0].body.roles.map(role => role.name), ["tester"]);
  assert.deepEqual(h.requests[0].body.execution_plan.roles[0].depends_on, []);
});

test("blank/duplicate names and cyclic dependencies are refused without dropping a member", async () => {
  for (const scenario of ["blank", "duplicate", "cycle"]) {
    const h = harness(); h.open(); h.add();
    if (scenario === "blank") h.input("tester name").props.onChange({ target: { value: "" } });
    if (scenario === "duplicate") h.input("tester name").props.onChange({ target: { value: "builder" } });
    if (scenario === "cycle") {
      h.input("tester waits for builder").props.onChange({ target: { checked: true } });
      h.input("builder waits for tester").props.onChange({ target: { checked: true } });
    }
    await h.save(); assert.equal(h.requests.length, 0); assert.equal(h.refreshes, 0);
    assert.ok(h.render().some(node => node.props.role === "alert"));
  }
});

test("failed save retains the draft and effective policy failure; reopening reads fresh props", async () => {
  const h = harness(400); h.open(); h.add(); await h.save();
  assert.ok(h.input("tester name")); assert.equal(h.refreshes, 0);
  assert.match(JSON.stringify(h.render()), /Existing policy denied/);
  h.button("Close").props.onClick();
  h.props.roster[0].system_prompt = "Updated by a fresh Team response";
  h.open(); assert.equal(h.input("builder instructions").props.value, "Updated by a fresh Team response");
  assert.equal(h.input("tester name"), undefined);
});

test("live Team and missing plan cannot save or start work", async () => {
  for (const field of ["paused", "executionPlan"]) {
    const h = harness(); h.props[field] = field === "paused" ? false : null; h.open();
    assert.equal(h.button("Save org").props.disabled, true);
    await h.save(); assert.equal(h.requests.length, 0);
  }
});

test("archetype members also receive a matching native plan without external memory access", async () => {
  const h = harness(); h.open();
  h.render().find(node => node.props.title === "Add a pre-defined member archetype")
    .props.onChange({ target: { value: "qa" } });
  await h.save();
  assert.equal(h.requests[0].body.execution_plan.roles[1].name, "qa");
  assert.deepEqual(h.requests[0].body.execution_plan.roles[1].phases[0].capabilities, ["filesystem-read", "filesystem-write"]);
});

test("expected-work validation matches the server's UTF-8 byte boundaries", () => {
  for (const objective of ["", "a".repeat(19)]) {
    assert.match(helper.phaseObjectiveError(objective), /Add more detail/);
  }
  for (const objective of ["a".repeat(20), "a".repeat(1200), "é".repeat(600), "🧪".repeat(300)]) {
    assert.equal(helper.phaseObjectiveError(objective), null);
  }
  for (const objective of ["a".repeat(1201), "é".repeat(601), "🧪".repeat(301)]) {
    assert.match(helper.phaseObjectiveError(objective), /too long/);
  }
});

test("invalid expected work identifies the field, keeps the draft and never sends a rejected plan", async () => {
  for (const objective of ["short", "a".repeat(1309), "🧪".repeat(301)]) {
    const h = harness(); h.open();
    const label = "builder / build expected work";
    h.input(label).props.onChange({ target: { value: objective } });
    const field = h.input(label);
    assert.equal(field.props["aria-invalid"], true);
    const hint = h.render().find(node => node.props.id === field.props["aria-describedby"]);
    assert.equal(hint.props.children, helper.phaseObjectiveError(objective));
    await h.save();
    assert.equal(h.requests.length, 0);
    assert.equal(h.refreshes, 0);
    assert.equal(h.input(label).props.value, objective);
    assert.match(JSON.stringify(h.render()), /builder \/ build:/);
    h.input(label).props.onChange({ target: { value: "Build a self-contained package and verify startup." } });
    assert.equal(h.input(label).props["aria-invalid"], false);
    await h.save();
    assert.equal(h.requests.length, 1);
    assert.equal(h.requests[0].body.execution_plan.roles[0].phases[0].objective,
      "Build a self-contained package and verify startup.");
    assert.equal(h.refreshes, 1);
  }
});

test("member plan retains every unrelated plan field and clamps parallelism only when roles are removed", () => {
  const h = harness(); const plan = { ...h.original, max_parallel: 2 };
  assert.equal(helper.rosterExecutionPlan(plan, plan.roles).max_parallel, 1);
  assert.deepEqual(plain(helper.rosterExecutionPlan(h.original, h.original.roles)), h.original);
});
