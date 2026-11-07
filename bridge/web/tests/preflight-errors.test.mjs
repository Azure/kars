// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const ts = createRequire(import.meta.url)("typescript");
const plain = value => JSON.parse(JSON.stringify(value));
function load(path, imports, globals = {}) {
  const exports = {};
  const result = ts.transpileModule(readFileSync(new URL(`../src/${path}`, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
    reportDiagnostics: true,
  });
  assert.deepEqual(result.diagnostics, []);
  runInNewContext(result.outputText, { exports, Error, Headers, ...globals, require(name) {
    assert.ok(name in imports, `Unexpected import: ${name}`);
    return imports[name];
  } });
  return exports;
}
class BffError extends Error {
  constructor(code, status, message = code) { super(message); this.code = code; this.status = status; }
}
function action(result, roles = ["operator"]) {
  const calls = [];
  return { calls, ...load("lib/preflight-actions.ts", {
    "@/lib/config": { defaultNamespace: () => "work" },
    "@/lib/session": { currentPrincipal: async () => ({ roles }) },
    "@/lib/bff": { BffError, validatePackage: async (...args) => {
      calls.push(args);
      if (result instanceof Error) throw result;
      return result;
    } },
  }) };
}

for (const workload of ["team", "mission"]) {
  test(`${workload} preflight returns serializable failed checks for malformed packages`, async () => {
    const api = action(new BffError("http_422", 422, "private parser value"));
    const result = await api.validatePackageAction({ execution_plan: { maxParallel: 2 } }, { workload });
    assert.equal(result.ok, false);
    assert.equal(result.checks[0].status, "fail");
    assert.match(result.checks[0].detail, /snake_case.*max_parallel.*max_tool_calls/);
    assert.doesNotMatch(JSON.stringify(result), /private parser value/);
    assert.equal(api.calls.length, 1);
  });
  test(`${workload} successful checks and envelope pass through unchanged`, async () => {
    const expected = { ok: true, checks: [{ id: "model", status: "pass", label: "Model", detail: "Available" }] };
    const api = action(expected), blueprint = { runtime: "Hermes" }, envelope = { workload, budget_tokens: null, delegation_depth: 1 };
    assert.deepEqual(plain(await api.validatePackageAction(blueprint, envelope)), expected);
    assert.deepEqual(plain(api.calls), [["work", blueprint, envelope]]);
  });
}

test("expected validation errors remain useful; transport and upstream details stay private", async () => {
  for (const status of [400, 409, 422]) {
    const result = await action(new BffError("bad_request", status, "Role names must match the plan"))
      .validatePackageAction({});
    assert.equal(result.checks[0].detail, "Role names must match the plan");
  }
  for (const error of [new Error("PRIVATE"), new BffError("upstream", 502, "PRIVATE"),
    new BffError("upstream", 400, "PRIVATE")]) {
    const result = await action(error).validatePackageAction({});
    assert.equal(result.ok, false);
    assert.match(result.checks[0].detail, /unavailable/);
    assert.doesNotMatch(JSON.stringify(result), /PRIVATE/);
  }
  assert.match((await action(new BffError("unauthorized", 401)).validatePackageAction({})).checks[0].detail, /Sign in/);
  assert.match((await action(new BffError("forbidden", 403)).validatePackageAction({})).checks[0].detail, /permissions/);
});

test("qualification authorization remains ahead of backend validation", async () => {
  const api = action({ ok: true, checks: [] }, ["viewer"]);
  const result = await api.validatePackageAction({}, { qualification_run: true });
  assert.equal(result.ok, false);
  assert.equal(result.checks[0].id, "qualification_authorization");
  assert.equal(api.calls.length, 0);
});

test("plain-text schema rejections become useful errors without exposing the raw body", async () => {
  const { createTeam, BffError: ClientError } = load("lib/bff.ts", {
    "./config": { bffBaseUrl: () => "http://bff" },
    "next/headers": { cookies: () => assert.fail("SSO disabled for this response fixture") },
    "./session-token": {}, "./oidc-config": { ssoConfigured: () => false },
    "./credential-review": {},
  }, { fetch: async () => new Response("Failed to deserialize: PRIVATE request value", { status: 422 }) });
  await assert.rejects(createTeam("work", {}), error => {
    assert.ok(error instanceof ClientError);
    assert.equal(error.status, 422);
    assert.match(error.message, /API schema/);
    assert.doesNotMatch(error.message, /PRIVATE/);
    return true;
  });
});

test("preflight forwards the exact repository selection for Mission and Team without creating a binding", async () => {
  const bodies = [];
  const { validatePackage } = load("lib/bff.ts", {
    "./config": { bffBaseUrl: () => "http://bff" },
    "next/headers": {}, "./session-token": {}, "./oidc-config": { ssoConfigured: () => false },
    "./credential-review": {},
  }, { fetch: async (_url, init) => { bodies.push(JSON.parse(init.body)); return Response.json({ ok: true, checks: [] }); } });
  for (const workload of ["mission", "team"]) {
    for (const selected of [[], ["Owner/Repo", "owner/other"]]) {
      const api = action({ ok: true, checks: [] });
      const envelope = { workload, git_write_repos: selected };
      await api.validatePackageAction({}, envelope);
      assert.deepEqual(plain(api.calls[0][2]), envelope);
      await validatePackage("work", {}, envelope);
      assert.deepEqual(bodies.at(-1).git_write_repos, selected);
      assert.equal(bodies.at(-1).github_binding, undefined);
    }
  }
});

test("preflight client serializes explicit delegation depth and preserves server defaults", async () => {
  const bodies = [];
  const { validatePackage } = load("lib/bff.ts", {
    "./config": { bffBaseUrl: () => "http://bff" },
    "next/headers": { cookies: () => assert.fail("SSO disabled for this request fixture") },
    "./session-token": {}, "./oidc-config": { ssoConfigured: () => false },
    "./credential-review": {},
  }, { fetch: async (_url, init) => {
    bodies.push(JSON.parse(init.body));
    return Response.json({ ok: true, checks: [] });
  } });
  for (const depth of [0, 1, 3, 2147483647]) {
    await validatePackage("work", {}, { workload: "team", delegation_depth: depth });
    assert.equal(bodies.at(-1).delegation_depth, depth);
  }
  await validatePackage("work", {}, { workload: "team" });
  assert.equal(Object.hasOwn(bodies.at(-1), "delegation_depth"), false);
});

test("Team composer forwards edited depth without inventing a grant", () => {
  const source = ts.createSourceFile("team-composer.tsx", readFileSync(new URL(
    "../src/app/workspace/teams/new/team-composer.tsx", import.meta.url), "utf8"),
  ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  let expression;
  function visit(node) {
    if (ts.isJsxSelfClosingElement(node) && node.tagName.getText(source) === "PreflightCheck") {
      const prop = node.attributes.properties.find(prop => prop.name?.getText(source) === "delegationDepth");
      expression = prop?.initializer?.expression?.getText(source);
    }
    ts.forEachChild(node, visit);
  }
  visit(source);
  assert.ok(expression, "Team preflight must include the reviewed depth");
  for (const [delegationDepth, expected] of [["", undefined], ["  ", undefined], ["0", 0], ["1", 1], ["3", 3]]) {
    assert.equal(runInNewContext(expression, { delegationDepth }), expected);
  }
});

test("budget failure shows aggregate and nested requested limits without rewriting the package", () => {
  const jsx = (type, props) => typeof type === "function" ? type(props) : ({ type, props: props ?? {} });
  const checks = load("components/validation-checks.tsx", {
    react: { useState: initial => [initial, () => assert.fail("No automatic changes")] },
    "react/jsx-runtime": { jsx, jsxs: jsx }, "@/lib/budget-review": load("lib/budget-review.ts", {}),
  });
  let result = { ok: false, checks: [{ id: "budget_contracts", status: "fail", label: "Budget", detail: "Missing model contract" }] };
  let cursor = 0;
  const { PreflightCheck } = load("components/preflight-check.tsx", {
    react: { useState: () => [cursor++ === 0 ? result : null, () => assert.fail("Must not change state")],
      useRef: value => ({ current: value }), useEffect() {},
      useTransition: () => [false, () => assert.fail("Must not validate automatically")] },
    "react/jsx-runtime": { jsx, jsxs: jsx },
    "@/lib/preflight-client": {}, "./validation-checks": checks,
  });
  function text(tree) {
    if (Array.isArray(tree)) return tree.map(text).join("");
    if (tree && typeof tree === "object") return text(tree.props?.children);
    return tree == null || typeof tree === "boolean" ? "" : String(tree);
  }
  const blueprint = { execution_plan: { roles: [
    { name: "benchmark-researcher", budget_tokens: 2000000 },
    { name: "application-engineer", budget_tokens: 2400000 },
    { name: "application-verifier", budget_tokens: 1800000 },
    { name: "inherited" }, { name: "bad-limit", budget_tokens: "2000" },
    null, { name: 7 },
  ] } };
  const before = JSON.stringify(blueprint);
  for (const workload of ["team", "mission"]) {
    cursor = 0;
    const content = text(PreflightCheck({ blueprint, budgetTokens: 8000000, workload }));
    for (const cap of ["8,000,000", "2,000,000", "2,400,000", "1,800,000"]) assert.ok(content.includes(`${cap} tokens`));
    assert.match(content, /Uses the shared limit/);
    assert.match(content, /Invalid token limit/);
    assert.match(content, /unbounded workspace allowance does not remove/);
    assert.equal(JSON.stringify(blueprint), before);
  }
  for (const value of [null, {}, { execution_plan: null }, { execution_plan: { roles: {} } }]) {
    cursor = 0;
    assert.match(text(PreflightCheck({ blueprint: value })), /No shared token limit requested/);
  }
  for (const status of ["pass", "warn"]) {
    result = { ok: true, checks: [{ id: "budget_contracts", status }] };
    cursor = 0;
    assert.doesNotMatch(text(PreflightCheck({ blueprint })), /Limits checked/);
  }
});

test("retry invalidates the previous successful UI result even if the action transport fails", async () => {
  const state = [], transitions = [], callbacks = [], envelopes = [];
  let cursor = 0, fail = false;
  const jsx = (type, props) => ({ type, props: props ?? {} });
  const { PreflightCheck } = load("components/preflight-check.tsx", {
    "react": {
      useState(initial) {
        const index = cursor++;
        if (!(index in state)) state[index] = initial;
        return [state[index], value => { state[index] = value; }];
      },
      useRef: value => ({ current: value }), useEffect() {},
      useTransition: () => [false, callback => transitions.push(callback())],
    },
    "react/jsx-runtime": { jsx, jsxs: jsx },
    "@/lib/preflight-client": { checkPackage: async (_blueprint, envelope) => {
      envelopes.push(envelope);
      if (fail) throw new Error("Transport unavailable");
      return { ok: true, checks: [] };
    } },
    "./validation-checks": { ValidationChecks: "Checks" },
  });
  function nodes(tree) {
    if (Array.isArray(tree)) return tree.flatMap(nodes);
    return tree?.props ? [tree, ...nodes(tree.props.children)] : [];
  }
  function render() { cursor = 0; return nodes(PreflightCheck({ blueprint: {}, workload: "team", delegationDepth: 1, onResult: value => callbacks.push(value) })); }
  render().find(node => node.type === "button").props.onClick();
  await Promise.all(transitions);
  assert.equal(callbacks.at(-1).ok, true);
  assert.equal(envelopes.at(-1).delegation_depth, 1);
  fail = true;
  render().find(node => node.type === "button").props.onClick();
  assert.equal(callbacks.at(-1).ok, false);
  await Promise.all(transitions);
  const tree = render();
  assert.equal(tree.some(node => node.type === "Checks"), false);
  assert.equal(tree.find(node => node.props.role === "alert").props.children, "Transport unavailable");
});
