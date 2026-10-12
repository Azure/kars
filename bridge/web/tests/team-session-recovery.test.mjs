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
    everyMinutes: null, lifecycleMode: "resourceOptimized", warmIdleSeconds: null,
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
