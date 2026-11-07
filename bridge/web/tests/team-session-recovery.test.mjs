// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const require = createRequire(import.meta.url);
const ts = require("typescript");
function load(path, globals = {}) {
  const exports = {};
  runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, { exports, URL, ...globals });
  return exports;
}
const auth = load("../src/lib/auth-return.ts");
function elements(node) {
  if (Array.isArray(node)) return node.flatMap(elements);
  if (!node || typeof node !== "object" || !node.props) return [];
  return [node, ...elements(node.props.children)];
}
function harness(status) {
  const states = [], requests = [], transitions = [];
  let cursor = 0, refreshes = 0;
  const props = {
    ns: "kars-system", name: "clinic", paused: true, charter: "Original charter",
    everyMinutes: null,
    mcpServers: [], availableMcp: [], egress: [], egressMode: null,
    model: null, modelFallbacks: [], models: [], memory: null,
    availableMemories: [], gitWriteRepos: [], executionPlan: null,
  };
  const { TeamEdit } = load("../src/app/workspace/teams/[name]/team-edit.tsx", {
    fetch: async (...args) => {
      requests.push(args);
      return new Response(JSON.stringify({ error: { message: "Request rejected" } }), { status });
    },
    require: name => {
      if (name === "react") return {
        useState(initial) {
          const index = cursor++;
          if (!(index in states)) states[index] = typeof initial === "function" ? initial() : initial;
          return [states[index], value => { states[index] = typeof value === "function" ? value(states[index]) : value; }];
        },
        useTransition: () => [false, callback => transitions.push(callback())],
      };
      if (name === "next/navigation") return { useRouter: () => ({ refresh: () => refreshes++ }) };
      if (name === "@/lib/auth-return") return auth;
      if (name === "@/components/repo-access") return { RepoAccess: () => null };
      return require(name);
    },
  });
  return {
    requests, props, render() { cursor = 0; return elements(TeamEdit(props)); },
    async settle() { await Promise.all(transitions); },
    get refreshes() { return refreshes; },
  };
}

function modelSelect(h) {
  const label = h.render().find(node => node.type === "label"
    && node.props.children[0] === "Principal/default model");
  return elements(label).find(node => node.type === "select");
}

function openEditor(h) {
  h.render().find(node => node.type === "button" && node.props.children === "Edit").props.onClick();
}

async function saveEditor(h) {
  h.render().find(node => node.type === "button" && node.props.children === "Save launch package").props.onClick();
  await h.settle();
  return JSON.parse(h.requests.at(-1)[1].body);
}

test("Team repository access follows the edited plan and blank edits preserve the saved plan", async () => {
  const saved = { roles: [], synthesis: { capabilities: ["network"] } };
  for (const text of [JSON.stringify({ ...saved, synthesis: { capabilities: ["github-pr"] } }), "", "{"]) {
    const h = harness(200);
    h.props.executionPlan = saved;
    h.props.gitWriteRepos = ["owner/demo"];
    openEditor(h);
    const repoAccess = () => h.render().find(node => node.type?.name === "RepoAccess");
    assert.deepEqual(JSON.parse(JSON.stringify(repoAccess().props.plan)), saved);
    assert.deepEqual(repoAccess().props.initialSelected, ["owner/demo"]);
    h.render().find(node => node.type === "textarea" && node.props.rows === 14)
      .props.onChange({ target: { value: text } });
    const expected = text && text !== "{" ? JSON.parse(text) : saved;
    assert.deepEqual(JSON.parse(JSON.stringify(repoAccess().props.plan)), expected);
    if (text === "{") {
      h.render().find(node => node.type === "button" && node.props.children === "Save launch package").props.onClick();
      await h.settle();
      assert.equal(h.requests.length, 0);
      assert.ok(h.render().some(node => node.props.children === "Typed execution plan must be valid JSON."));
    } else {
      const body = await saveEditor(h);
      assert.deepEqual(body.git_write_repos, ["owner/demo"]);
      if (text) assert.deepEqual(body.execution_plan, expected);
      else assert.equal("execution_plan" in body, false);
    }
  }
});

test("Team edits enforce Strict and preserve reviewed external hosts", async () => {
  for (const legacyMode of [null, "Learn", "Strict"]) {
    const h = harness(200);
    h.props.egressMode = legacyMode;
    h.props.egress = ["docs.example.com:443"];
    openEditor(h);
    const tree = h.render();
    assert.match(JSON.stringify(tree), /Egress: Strict/);
    assert.match(JSON.stringify(tree), /Datapath observations do not grant access/);
    assert.equal(tree.some(node => node.type === "option" && node.props.value === "learning"), false);
    const body = await saveEditor(h);
    assert.equal(body.egress_mode, "strict");
    assert.deepEqual(body.egress, [{ host: "docs.example.com", port: 443 }]);
  }
});

test("Team edits preserve a configured route absent from the current catalog", async () => {
  for (const models of [[], [{ provider: "other-provider", deployment: "other-model" }]]) {
    const h = harness(200);
    h.props.model = "retained-provider::retained-model";
    h.props.models = models;
    openEditor(h);
    const select = modelSelect(h);
    assert.equal(select.props.value, h.props.model);
    const retained = elements(select).find(node => node.type === "option" && node.props.value === h.props.model);
    assert.ok(retained, "the configured route must remain visible rather than displaying cluster default");
    assert.match(JSON.stringify(retained.props.children), /not in current catalog/);
    h.render().find(node => node.type === "textarea").props.onChange({ target: { value: "Updated charter" } });
    const body = await saveEditor(h);
    assert.equal(body.charter, "Updated charter");
    assert.equal(body.model, h.props.model, "an unrelated save must not clear the existing model");
  }
});

test("Team model changes require an explicit selection, including cluster default", async () => {
  for (const value of ["", "other-provider::other-model"]) {
    const h = harness(200);
    h.props.model = "retained-provider::retained-model";
    h.props.models = [{ provider: "other-provider", deployment: "other-model" }];
    openEditor(h);
    modelSelect(h).props.onChange({ target: { value } });
    assert.equal(modelSelect(h).props.value, value);
    assert.equal((await saveEditor(h)).model, value);
  }
});

test("Team model catalog keeps qualified, legacy and cluster-default selections", () => {
  for (const [model, expected] of [["provider::model", "provider::model"], ["model", "provider::model"], [null, ""]]) {
    const h = harness(200);
    h.props.model = model;
    h.props.models = [{ provider: "provider", deployment: "model" }];
    openEditor(h);
    const select = modelSelect(h);
    assert.equal(select.props.value, expected);
    assert.equal(elements(select).filter(node => node.type === "option" && node.props.value === expected).length, 1);
  }
});

test("Team Resume 401 retains edits and offers document sign-in without replaying the mutation", async () => {
  const h = harness(401);
  h.render().find(node => node.type === "button" && node.props.children === "Edit").props.onClick();
  h.render().find(node => node.type === "textarea").props.onChange({ target: { value: "Unsaved charter" } });
  h.render().find(node => node.type === "button" && node.props.children === "Resume").props.onClick();
  await h.settle();
  const nodes = h.render();
  const signIn = nodes.find(node => node.type === "a" && node.props.children === "Sign in again");
  assert.ok(signIn, "401 must offer a usable full-page sign-in link, not just a save error");
  const url = new URL(signIn.props.href, "https://bridge.example");
  assert.equal(url.pathname, "/auth/login");
  assert.equal(url.searchParams.get("returnTo"), "/workspace/teams/clinic");
  assert.equal(signIn.props.onClick, undefined, "ordinary anchor uses document navigation");
  assert.ok(nodes.some(node => node.props.role === "alert"));
  assert.equal(nodes.find(node => node.type === "textarea").props.value, "Unsaved charter");
  assert.equal(h.requests.length, 1);
  assert.equal(h.requests[0][0], "/api/namespaces/kars-system/teams/clinic");
  assert.deepEqual(JSON.parse(h.requests[0][1].body), { paused: false });
  assert.equal(h.refreshes, 0);
  assert.ok(nodes.some(node => node.type === "button" && node.props.children === "Resume"));
});

test("Team forbidden and service failures do not become authentication recovery", async () => {
  for (const status of [403, 409, 500]) {
    const h = harness(status);
    h.render().find(node => node.type === "button" && node.props.children === "Resume").props.onClick();
    await h.settle();
    assert.equal(h.render().some(node => node.type === "a"), false);
    assert.ok(h.render().some(node => node.props.children === "Request rejected"));
    assert.equal(h.requests.length, 1);
    assert.equal(h.refreshes, 0);
  }
});

test("Team Resume success keeps the maintained refresh path", async () => {
  const h = harness(200);
  h.render().find(node => node.type === "button" && node.props.children === "Resume").props.onClick();
  await h.settle();
  assert.equal(h.refreshes, 1);
  assert.equal(h.requests.length, 1);
  assert.equal(h.render().some(node => node.type === "a"), false);
});

test("Team edits do not offer or serialize unsupported warm runtime settings", async () => {
  const h = harness(200);
  h.render().find(node => node.type === "button" && node.props.children === "Edit").props.onClick();
  assert.equal(h.render().some(node =>
    node.type === "option" && ["resourceOptimized", "persistent"].includes(node.props.value)), false);
  assert.ok(h.render().some(node =>
    node.type === "p" && JSON.stringify(node.props.children).includes("not currently supported")));
  h.render().find(node => node.type === "button" && node.props.children === "Save launch package").props.onClick();
  await h.settle();
  assert.equal(h.requests.length, 1);
  const body = JSON.parse(h.requests[0][1].body);
  assert.equal(body.charter, h.props.charter);
  assert.equal("lifecycle_mode" in body, false);
  assert.equal("warm_idle_seconds" in body, false);
  assert.equal(h.refreshes, 1);
});
