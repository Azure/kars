// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const ts = createRequire(import.meta.url)("typescript");
function load(path, imports, globals = {}) {
  const context = { exports: {}, ...globals, require(name) {
    assert.ok(name in imports, `Unexpected import: ${name}`);
    return imports[name];
  } };
  runInNewContext(ts.transpileModule(readFileSync(new URL(`../src/${path}`, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022,
      jsx: ts.JsxEmit.ReactJSX },
  }).outputText, context);
  return context.exports;
}
const plain = value => JSON.parse(JSON.stringify(value));
const model = { provider: "azure-foundry", deployment: "gpt-5.4-mini" };
function form(overrides = {}) {
  const data = new FormData();
  for (const [key, value] of Object.entries({ objective: "Write an internal privacy brief", tier: "1",
    blueprint_json: JSON.stringify({ runtime: "OpenClaw", model }),
    budget_tokens: "2000000", budget_scope: "GovernedInference", ...overrides })) {
    if (value !== null) data.set(key, value);
  }
  return data;
}
class TestBffError extends Error {
  constructor(code, status, message) {
    super(message);
    this.code = code;
    this.status = status;
  }
}
function intake(createError, validationError) {
  const creates = [], validations = [];
  const api = load("app/workspace/new/actions.ts", {
    "next/navigation": { redirect: path => { throw Object.assign(new Error("redirect"), { path }); } },
    "@/lib/config": { defaultNamespace: () => "test-workspace" },
    "@/lib/session": { currentPrincipal: async () => ({ name: "reviewer" }) },
    "@/lib/bff": {
      BffError: TestBffError,
      createTask: async (...args) => {
        creates.push(plain(args));
        if (createError) throw createError;
      },
      validatePackage: async (...args) => {
        validations.push(plain(args));
        if (validationError) throw validationError;
        return { ok: true, checks: [] };
      },
    },
  });
  return { ...api, creates, validations };
}
async function create(api, data) {
  await assert.rejects(api.createMissionAction({ error: null }, data), error =>
    error.path?.startsWith("/workspace/missions/") === true);
  return api.creates.at(-1)[1];
}

test("new finite mission preserves the reviewed scope, cap, model and draft gate", async () => {
  const api = intake();
  const body = await create(api, form());
  assert.deepEqual(body.envelope.budget, { scope: "GovernedInference", tokens: 2000000, usd_micros: null });
  assert.deepEqual(body.blueprint.model, model);
  assert.equal(body.launch, false);
  assert.equal(body.created_by, "reviewer");
  assert.equal(api.creates[0][0], "test-workspace");
});

test("typed reviewed missions retain two delegation levels without changing legacy missions", async () => {
  for (const [execution_plan, delegation, depth] of [
    [{ version: "v1", roles: [{ name: "writer" }] }, { mode: "single" }, 2],
    [{ version: "v1", roles: [{ name: "writer" }] }, { mode: "principal-specialists", specialists: [] }, 2],
    [undefined, { mode: "principal-specialists", specialists: [] }, 1],
    [undefined, { mode: "single" }, 0],
  ]) {
    for (const launch of [null, "on"]) {
      const api = intake();
      const body = await create(api, form({
        blueprint_json: JSON.stringify({ runtime: "Hermes", model, execution_plan }),
        delegation_json: JSON.stringify(delegation), launch,
      }));
      assert.equal(body.envelope.delegation_depth, depth);
      assert.equal(body.envelope.budget.tokens, 2000000);
      assert.equal(body.blueprint.runtime, "Hermes");
      assert.equal(body.launch, launch === "on");
    }
  }
});

test("currency-only and combined reviewed caps are retained", async () => {
  for (const tokens of [null, "2000000"]) {
    const api = intake();
    const body = await create(api, form({ budget_tokens: tokens, budget_usd_micros: "5000000", launch: "on" }));
    assert.deepEqual(body.envelope.budget, { scope: "GovernedInference", tokens: tokens ? 2000000 : null, usd_micros: 5000000 });
    assert.equal(body.launch, true);
  }
});

test("positive unscoped or unknown-scope caps never create a mission", async () => {
  for (const scope of [null, "", "Planning", "governedinference"]) {
    const api = intake();
    const result = await api.createMissionAction({}, form({ budget_scope: scope }));
    assert.match(result.error, /explicit GovernedInference/);
    assert.equal(api.creates.length, 0);
  }
});

test("invalid limits are rejected rather than truncated or silently made unbounded", async () => {
  for (const field of ["budget_tokens", "budget_usd_micros"]) {
    for (const value of ["-1", "0", "0.5", "2.3", "NaN", "Infinity", "not-a-number", "9007199254740992"]) {
      const api = intake();
      const result = await api.createMissionAction({}, form({ [field]: value }));
      assert.match(result.error, /positive safe whole numbers/);
      assert.equal(api.creates.length, 0);
    }
  }
});

test("malformed reviewed JSON never creates a default mission", async () => {
  for (const overrides of [
    { blueprint_json: "{" }, { blueprint_json: "null" },
    { blueprint_json: "[]" }, { blueprint_json: '"wrong"' },
    { delegation_json: "{" },
  ]) {
    const api = intake();
    const result = await api.createMissionAction({}, form(overrides));
    assert.match(result.error, /JSON/);
    assert.equal(api.creates.length, 0);
  }
});

test("create errors explain typed validation without exposing unstructured upstream bodies", async () => {
  for (const code of ["bad_request", "rejected", "conflict"]) {
    const api = intake(new TestBffError(code, 400, "Review the execution plan capabilities."));
    const result = await api.createMissionAction({}, form());
    assert.equal(result.error, "Review the execution plan capabilities.");
  }
  const invalid = intake(new TestBffError("http_422", 422, "private upstream body"));
  const result = await invalid.createMissionAction({}, form());
  assert.match(result.error, /invalid fields.*field types/);
  assert.doesNotMatch(result.error, /private/);
  const upstream = intake(new TestBffError("upstream", 500, "private credential"));
  assert.doesNotMatch((await upstream.createMissionAction({}, form())).error, /private/);
});

test("blank limits are explicit absence, not implicit enrollment", async () => {
  const api = intake();
  const body = await create(api, form({ budget_tokens: " ", budget_scope: "" }));
  assert.equal(body.envelope.budget, null);
  const result = await api.createMissionAction({}, form({ budget_tokens: "" }));
  assert.match(result.error, /positive reviewed limit/);
  assert.equal(api.creates.length, 1);
});

test("Home preflight forwards exactly the reviewed envelope", async () => {
  const api = intake();
  const envelope = { tier: 1, budget_tokens: 2000000, budget_usd_micros: 5000000,
    budget_scope: "GovernedInference" };
  await api.validateMissionAction({ model }, envelope);
  assert.deepEqual(api.validations, [["test-workspace", { model }, envelope]]);
});

test("Home preflight renders expected schema and launch errors as failed checks", async () => {
  for (const error of [
    new TestBffError("bad_request", 400, "TypedPlanExecutionUnavailable: review the plan."),
    new TestBffError("http_422", 422, "private unstructured body"),
    new TestBffError("bad_request", 500, "private upstream detail"),
    new Error("private credential"),
  ]) {
    const result = await intake(undefined, error).validateMissionAction({ model });
    assert.equal(result.ok, false);
    assert.equal(result.checks[0].status, "fail");
    assert.doesNotMatch(JSON.stringify(result), /private/);
    if (error.status === 400) assert.match(result.checks[0].detail, /TypedPlanExecutionUnavailable/);
    if (error.status === 422) assert.match(result.checks[0].detail, /invalid fields/);
  }
});

test("BFF serializes all budget fields through its authenticated no-store POST", async () => {
  const requests = [];
  const api = load("lib/bff.ts", {
    "./config": { bffBaseUrl: () => "http://bff" },
    "next/headers": { cookies: async () => ({ get: () => ({ value: "signed-test-session" }) }) },
    "./session-token": { SESSION_COOKIE: "test", verifySession: async () => true },
    "./oidc-config": { ssoConfigured: () => true },
    "./credential-review": {},
  }, { Headers, fetch: async (...args) => {
    requests.push(args); return { ok: true, json: async () => ({ ok: true, checks: [] }) };
  } });
  const envelope = { tier: 1, budget_tokens: 2000000, budget_usd_micros: 5000000,
    budget_scope: "GovernedInference", workload: "mission" };
  await api.validatePackage("workspace/a", { model }, envelope);
  const [url, init] = requests[0];
  assert.equal(url, "http://bff/api/namespaces/workspace%2Fa/validate");
  assert.equal(init.method, "POST");
  assert.equal(init.cache, "no-store");
  assert.equal(init.headers.get("x-kars-principal-token"), "signed-test-session");
  assert.deepEqual(JSON.parse(init.body), { blueprint: { model }, ...envelope });
});

test("review form explicitly includes scope and describes lifetime and reservation semantics", () => {
  const jsx = (type, props) => ({ type, props });
  const imports = { "react/jsx-runtime": { jsx, jsxs: jsx } };
  for (const name of ["@/components/segmented-tier", "@/components/orchestration-cube",
    "@/components/validation-checks",
    "@/components/icon", "@/components/journey-rail", "@/lib/format", "@/components/repo-access",
    "../envelope-reveal", "./controls", "./helpers"]) imports[name] = {};
  const { renderReview } = load("app/workspace/new/intake-flow/review.tsx", imports);
  function visit(node) {
    if (Array.isArray(node)) return node.flatMap(visit);
    if (!node || typeof node !== "object") return [];
    return [node, ...visit(node.props?.children)];
  }
  for (const tokens of ["2000000", ""]) {
    const nodes = visit(renderReview({ budgetTokens: tokens, tier: 1, blueprint: { model },
      delegation: { mode: "single" }, objective: "Brief", model: "azure-foundry::gpt-5.4-mini",
      options: { models: [], runtimes: [], tool_policies: [], mcp_servers: [], mcp_profiles: [], skills: [], memories: [], isolation: [] },
      modelFallbacks: [], mcp: [], skills: [], egress: [], recommendedStats: null,
      loopDirective: "", executionPlanDraft: "", executionPlanError: null, state: { error: null } }));
    assert.equal(nodes.find(node => node.props?.name === "budget_scope").props.value,
      tokens ? "GovernedInference" : "");
    assert.equal(nodes.find(node => node.props?.name === "budget_tokens").props.value, tokens);
    assert.match(JSON.stringify(nodes), /across retries and reruns/);
    assert.match(JSON.stringify(nodes), /full input bound/);
  }
});
