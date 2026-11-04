// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const ts = createRequire(import.meta.url)("typescript");
const plain = value => JSON.parse(JSON.stringify(value));
const jsx = (type, props) => ({ type, props: props ?? {} });
function load(path, imports = {}, globals = {}) {
  const exports = {};
  const result = ts.transpileModule(readFileSync(new URL(`../src/${path}`, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
    reportDiagnostics: true,
  });
  assert.deepEqual(result.diagnostics, []);
  runInNewContext(result.outputText, { exports, Error, ...globals, require(name) {
    if (name === "react/jsx-runtime") return { jsx, jsxs: jsx };
    assert.ok(name in imports, `Unexpected import ${name}`);
    return imports[name];
  } });
  return exports;
}
function nodes(tree) {
  if (Array.isArray(tree)) return tree.flatMap(nodes);
  if (!tree?.props) return [];
  return [tree, ...nodes(tree.props.children)];
}
const { TeamRoleResourcePicker: Picker } = load("components/team-role-resources.tsx");
const available = { mcp_servers: ["docs", "other"], egress: [{ host: "docs.example", port: 443 }] };
const empty = { mcp_servers: [], egress: [] };

test("member picker distinguishes legacy from explicit empty and selects only the chosen resources", () => {
  let value;
  const render = () => nodes(Picker({ name: "reader", value, available, onChange: next => { value = next; } }));
  assert.match(JSON.stringify(render()), /Legacy selection unchanged/);
  render().find(node => node.type === "button").props.onClick();
  assert.deepEqual(plain(value), empty);
  assert.doesNotMatch(JSON.stringify(render()), /Legacy selection unchanged/);
  const inputs = () => render().filter(node => node.type === "input");
  inputs()[0].props.onChange({ target: { checked: true } });
  inputs()[2].props.onChange({ target: { checked: true } });
  assert.deepEqual(plain(value), { mcp_servers: ["docs"], egress: available.egress });
  inputs()[0].props.onChange({ target: { checked: false } });
  assert.deepEqual(plain(value), { mcp_servers: [], egress: available.egress });
});

test("removed Team resources remain selected and visibly invalid until explicitly removed", () => {
  let value = { mcp_servers: ["removed"], egress: [{ host: "DOCS.EXAMPLE" }, { host: "removed.example", port: 8443 }] };
  const render = () => nodes(Picker({ name: "reader", value, available, onChange: next => { value = next; } }));
  const labels = render().filter(node => node.type === "label");
  assert.equal(labels.length, 5);
  const invalid = labels.filter(node => JSON.stringify(node).includes("not selected for Team"));
  assert.equal(invalid.length, 2);
  for (const label of invalid) {
    assert.equal(nodes(label).find(node => node.type === "input").props.checked, true);
  }
  const removedServer = render().find(node => node.type === "label" && JSON.stringify(node).includes("MCP: ") && JSON.stringify(node).includes("removed"));
  nodes(removedServer).find(node => node.type === "input").props.onChange({ target: { checked: false } });
  assert.deepEqual(plain(value.mcp_servers), []);
  assert.equal(value.egress.length, 2);
});

test("member memory is opt-in and removed Team bindings stay visible until cleared", () => {
  let value = { ...empty };
  let resources = { ...available, memory: "shared-memory" };
  const render = () => nodes(Picker({ name: "reader", value, available: resources, onChange: next => { value = next; } }));
  const select = () => render().find(node => node.props["aria-label"] === "reader memory binding");
  assert.equal(select().props.value, "");
  select().props.onChange({ target: { value: "shared-memory" } });
  assert.deepEqual(plain(value), { ...empty, memory: "shared-memory" });
  resources = { ...available, memory: "replacement" };
  assert.equal(select().props.value, "shared-memory");
  assert.match(JSON.stringify(render()), /not selected for Team/);
  assert.equal(render().find(node => node.type === "option" && node.props.value === "shared-memory").props.disabled, true);
  select().props.onChange({ target: { value: "" } });
  assert.equal(value.memory, null);
  assert.doesNotMatch(JSON.stringify(render()), /not selected for Team/);
});

test("existing roster editor retains member bindings, supports explicit empty and preserves omitted legacy fields", async () => {
  const state = [], transitions = [], requests = [];
  let cursor = 0;
  const react = {
    useState(initial) {
      const index = cursor++;
      if (!(index in state)) state[index] = typeof initial === "function" ? initial() : initial;
      return [state[index], next => { state[index] = typeof next === "function" ? next(state[index]) : next; }];
    },
    useTransition: () => [false, callback => transitions.push(callback())],
  };
  const { TeamRosterEdit } = load("app/workspace/teams/[name]/team-roster-edit.tsx", {
    react, "@/lib/member-archetypes": { MEMBER_ARCHETYPES: [] },
    "next/navigation": { useRouter: () => ({ refresh() {} }) },
    "@/components/viewport-portal": { ViewportPortal: () => null },
    "@/components/team-role-resources": { TeamRoleResourcePicker: Picker },
    "@/lib/team-roster-plan": load("lib/team-roster-plan.ts"),
  }, { fetch: async (url, init) => { requests.push({ url, body: JSON.parse(init.body) }); return { ok: true }; } });
  const selected = { mcp_servers: ["docs"], egress: available.egress };
  const props = { ns: "work", name: "team", resources: available, paused: true,
    executionPlan: { schema: "kars.execution-plan/v1", max_parallel: 1,
      roles: ["reader", "legacy", "memory-reader"].map(name => ({ name,
        objective: "Read and report findings", depends_on: [], phases: [] })),
      synthesis: { objective: "Package and report findings", capabilities: [], max_tool_calls: 50, execution_mode: "native" },
      deliverables: [] },
    roster: [{ name: "reader", skills: [], resources: selected }, { name: "legacy", skills: [] },
      { name: "memory-reader", skills: [], resources: { ...empty, memory: "shared-memory" } }],
    options: { models: [], runtimes: [], skills: [] } };
  const render = () => { cursor = 0; return nodes(TeamRosterEdit(props)); };
  render().find(node => node.type === "button").props.onClick();
  let pickers = render().filter(node => node.type === Picker);
  assert.deepEqual(plain(pickers[0].props.value), selected);
  assert.equal(pickers[1].props.value, undefined);
  pickers[0].props.onChange(empty);
  render().find(node => node.type === "button" && JSON.stringify(node.props.children).includes("Save")).props.onClick();
  await Promise.all(transitions);
  assert.deepEqual(requests[0].body.roles[0].resources, empty);
  assert.equal("resources" in requests[0].body.roles[1], false);
  assert.deepEqual(requests[0].body.roles[2].resources, { ...empty, memory: "shared-memory" });
});

test("Team default memory describes backend selection without replacing an explicit binding", () => {
  const { renderGovernancePanel } = load("app/workspace/teams/new/team-composer-panels.tsx", {
    "@/components/icon": { Icon: () => null },
    "@/components/team-role-resources": { TeamRoleResourcePicker: Picker },
    "@/lib/member-archetypes": { MEMBER_ARCHETYPES: [] },
  });
  const binding = { name: "stable-memory", backend: "foundry", readiness: "Ready" };
  let memory = "";
  const render = () => renderGovernancePanel({
    name: "desk", memory, setMemory: value => { memory = value; },
    selectedMemoryOption: memory ? binding : undefined,
    options: { tool_policies: [], memories: [binding], runtimes: [], models: [], mcp_servers: [] },
    mcp: [], modelFallbacks: [],
  });
  const select = tree => nodes(tree).find(node => node.props["aria-label"] === "Team runtime memory");
  let tree = render();
  assert.equal(select(tree).props.value, "");
  assert.match(JSON.stringify(tree), /Egress: Strict/);
  assert.match(JSON.stringify(tree), /Datapath observations do not grant access/);
  assert.equal(nodes(tree).some(node => node.type === "option" && node.props.value === "learning"), false);
  assert.match(JSON.stringify(tree), /Default — Foundry when configured, otherwise built-in/);
  assert.match(JSON.stringify(tree), /Private runtime memory, separate/);
  assert.match(JSON.stringify(tree), /Foundry failures do not switch to built-in memory/);
  assert.doesNotMatch(JSON.stringify(tree), /None — rely/);
  select(tree).props.onChange({ target: { value: binding.name } });
  tree = render();
  assert.equal(select(tree).props.value, binding.name);
  assert.match(JSON.stringify(tree), /Ready/);
  assert.doesNotMatch(JSON.stringify(tree), /Private runtime memory, separate/);
  select(tree).props.onChange({ target: { value: "" } });
  assert.equal(memory, "");
});
