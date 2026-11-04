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
function load(path, imports) {
  const exports = {};
  const compiled = ts.transpileModule(readFileSync(new URL(`../src/${path}`, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
    reportDiagnostics: true,
  });
  assert.deepEqual(compiled.diagnostics, []);
  runInNewContext(compiled.outputText, { exports, Error, require(name) {
    if (name === "react/jsx-runtime") return { jsx, jsxs: jsx };
    assert.ok(name in imports, name);
    return imports[name];
  } });
  return exports;
}
function nodes(tree) {
  if (Array.isArray(tree)) return tree.flatMap(nodes);
  return tree?.props ? [tree, ...nodes(tree.props.children)] : [];
}
function hooks() {
  const state = [], effects = [], transitions = [];
  let cursor = 0;
  const react = {
    useState(initial) {
      const i = cursor++;
      if (!(i in state)) state[i] = typeof initial === "function" ? initial() : initial;
      return [state[i], value => {
        assert.notEqual(typeof value, "function", "selection callbacks must not run in a state updater");
        state[i] = value;
      }];
    },
    useRef(initial) { const i = cursor++; return state[i] ??= { current: initial }; },
    useEffect(effect) { effects.push(effect); },
    useTransition: () => [false, callback => transitions.push(callback())],
  };
  return { react, effects, transitions, render(component, props) { cursor = 0; return nodes(component(props)); } };
}

test("repository checkboxes and the submitted hidden field use the same controlled set across remounts", () => {
  let selected = ["Owner/Repo"];
  function mount() {
    const h = hooks();
    const { RepoAccess } = load("components/repo-access.tsx", {
      react: h.react, "@/lib/config": { defaultNamespace: () => "work" },
    });
    return () => h.render(RepoAccess, { selected, initialSelected: ["wrong/repo"], onSelectionChange: repos => { selected = plain(repos); } });
  }
  let render = mount();
  assert.equal(render().find(node => node.props.name === "git_write_repos").props.value, "Owner/Repo");
  // Connection fetching is intentionally not run; selected rows must still be retained.
  selected = ["owner/other", "Owner/Repo"];
  assert.equal(render().find(node => node.props.name === "git_write_repos").props.value, selected.join(","));
  render = mount();
  assert.equal(render().find(node => node.props.name === "git_write_repos").props.value, selected.join(","));
  selected = [];
  assert.equal(render().find(node => node.props.name === "git_write_repos").props.value, "");
});

test("uncontrolled repository editing retains its existing selection without updater side effects", () => {
  const h = hooks();
  // Seed a loaded connection as the first component state without using a real network.
  const react = { ...h.react, useState(initial) {
    return h.react.useState(initial === null ? { connected: true, repos: ["Owner/Repo"], account: "owner" } : initial);
  } };
  const changes = [];
  const { RepoAccess } = load("components/repo-access.tsx", { react, "@/lib/config": { defaultNamespace: () => "work" } });
  const render = () => h.render(RepoAccess, { initialSelected: ["owner/repo"], onSelectionChange: repos => changes.push(plain(repos)) });
  assert.equal(render().find(node => node.props.type === "checkbox").props.checked, true);
  render().find(node => node.props.type === "checkbox").props.onChange();
  assert.deepEqual(changes, [[]]);
  assert.equal(render().find(node => node.props.name === "git_write_repos").props.value, "");
  render().find(node => node.props.type === "checkbox").props.onChange();
  assert.deepEqual(changes, [[], ["Owner/Repo"]]);
});

test("unmounted Team preflight cannot publish a late approval for an old repository selection", async () => {
  const h = hooks(), calls = [], results = [];
  let resolve;
  const { PreflightCheck } = load("components/preflight-check.tsx", {
    react: h.react,
    "@/lib/preflight-client": { checkPackage: (...args) => {
      calls.push(args); return new Promise(done => { resolve = done; });
    } },
    "./validation-checks": { ValidationChecks: "ValidationChecks" },
  });
  const tree = h.render(PreflightCheck, { blueprint: {}, workload: "team", selectedRepos: ["owner/repo"], onResult: result => results.push(plain(result)) });
  const unmount = h.effects[0]();
  tree.find(node => node.type === "button").props.onClick();
  assert.deepEqual(plain(calls[0][1].git_write_repos), ["owner/repo"]);
  unmount();
  resolve({ ok: true, checks: [] });
  await Promise.all(h.transitions);
  assert.deepEqual(results, [{ ok: false, checks: [] }]);
});
