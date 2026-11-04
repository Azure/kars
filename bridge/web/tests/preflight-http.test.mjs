// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const ts = createRequire(import.meta.url)("typescript");
const plain = value => JSON.parse(JSON.stringify(value));
function load(path, imports = {}, globals = {}) {
  const exports = {};
  const result = ts.transpileModule(readFileSync(new URL(`../src/${path}`, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 }, reportDiagnostics: true,
  });
  assert.deepEqual(result.diagnostics, []);
  runInNewContext(result.outputText, { exports, Error, Response, URL, TextDecoder, ...globals, require(name) {
    assert.ok(name in imports, `Unexpected import: ${name}`);
    return imports[name];
  } });
  return exports;
}
const passed = { ok: true, checks: [{ id: "model", label: "Model", status: "pass", detail: "Available" }] };
function route({ roles = ["user"], result = passed } = {}) {
  const calls = [];
  const validate = surface => async (...args) => {
    calls.push({ surface, args: plain(args) });
    if (result instanceof Error) throw result;
    return result;
  };
  return { calls, ...load("app/api/preflight/route.ts", {
    "@/lib/preflight-actions": { validatePackageAction: validate("team") },
    "@/app/workspace/new/actions": { validateMissionAction: validate("mission") },
    "@/lib/request-roles": { requestRoles: async get => { assert.equal(get("session"), "signed-fixture"); return roles; } },
    "@/lib/oidc-config": { ssoConfigured: () => true },
  }) };
}
function request(body = {}, headers = {}) {
  const req = new Request("http://web:3000/api/preflight", {
    method: "POST", body: typeof body === "string" ? body : JSON.stringify(body),
    headers: { origin: "https://bridge.example", host: "bridge.example", "content-type": "application/json", ...headers },
  });
  req.cookies = { get: () => ({ value: "signed-fixture" }) };
  return req;
}

test("stable route preserves both validators, explicit limits, roles and repository selections", async () => {
  for (const workload of [undefined, "mission", "team"]) {
    const api = route();
    const body = { blueprint: { runtime: "Hermes", model: { provider: "copilot", deployment: "model" } },
      envelope: { workload, tier: 2, delegation_depth: 1, budget_tokens: 9000000,
        budget_scope: "GovernedInference", budget_usd_micros: null, qualification_run: false,
        team_roles: workload === "team" ? [{ name: "researcher", budget_tokens: 2000000 }] : undefined,
        git_write_repos: ["owner/repo"] } };
    const response = await api.POST(request(body));
    assert.equal(response.status, 200);
    assert.equal(response.headers.get("cache-control"), "private, no-store");
    assert.deepEqual(await response.json(), passed);
    assert.deepEqual(api.calls, [{ surface: workload === "team" ? "team" : "mission", args: plain([body.blueprint, body.envelope]) }]);
  }
});

test("cross-origin, unauthenticated and malformed requests never reach validation", async () => {
  for (const origin of ["https://attacker.example", "null", "https://bridge.example/path", ""]) {
    const api = route();
    assert.equal((await api.POST(request({}, { origin }))).status, 403);
    assert.deepEqual(api.calls, []);
  }
  const unsigned = route({ roles: [] });
  assert.equal((await unsigned.POST(request())).status, 401);
  assert.deepEqual(unsigned.calls, []);
  const api = route();
  assert.equal((await api.POST(request({}, { "content-type": "text/plain" }))).status, 415);
  for (const body of ["{", "null", [], { namespace: "other" }, { envelope: [] }, { envelope: null },
    { envelope: { workload: "unknown" } }, { envelope: { qualification_run: "false" } }]) {
    assert.equal((await api.POST(request(body))).status, 400);
  }
  assert.deepEqual(api.calls, []);
});

test("the request size limit counts streamed UTF-8 bytes without trusting content-length", async () => {
  const api = route();
  const response = await api.POST(request({ blueprint: { instructions: "é".repeat(600000) } }, { "content-length": "1" }));
  assert.equal(response.status, 413);
  assert.deepEqual(api.calls, []);
});

test("authorization denials and failed checks survive intact; unexpected errors stay private", async () => {
  const denied = { ok: false, checks: [{ id: "qualification_authorization", label: "Denied", status: "fail", detail: "Only an operator or admin may designate a qualification run." }] };
  assert.deepEqual(await (await route({ result: denied }).POST(request({ envelope: { qualification_run: true } }))).json(), denied);
  const response = await route({ result: new Error("PRIVATE") }).POST(request());
  assert.equal(response.status, 503);
  assert.doesNotMatch(await response.text(), /PRIVATE/);
});

test("browser checks use the stable endpoint, retain the exact package and need no action identifiers", async () => {
  const calls = [], body = { blueprint: { execution_plan: { roles: [{ name: "researcher", budget_tokens: 2000000 }] } },
    envelope: { workload: "team", budget_tokens: 9000000, qualification_run: true } };
  const before = JSON.stringify(body);
  const client = load("lib/preflight-client.ts", {}, { fetch: async (...args) => { calls.push(args); return Response.json(passed); } });
  for (let build = 0; build < 2; build++) {
    assert.deepEqual(plain(await client.checkPackage(body.blueprint, body.envelope)), passed);
  }
  assert.equal(JSON.stringify(body), before);
  for (const [url, init] of calls) {
    assert.equal(url, "/api/preflight");
    assert.equal(init.method, "POST");
    assert.equal(init.credentials, "same-origin");
    assert.equal(init.redirect, "error");
    assert.equal(init.cache, "no-store");
    assert.deepEqual(JSON.parse(init.body), body);
    assert.equal(new Headers(init.headers).has("next-action"), false);
  }
});

test("browser transport and malformed responses fail closed without showing technical errors", async () => {
  const cases = [
    async () => { throw new Error('Server Action "PRIVATE" was not found'); },
    async () => new Response("PRIVATE", { status: 502 }),
    async () => new Response("PRIVATE"),
    async () => Response.json({ ok: true, checks: [] }),
    async () => Response.json({ ok: true, checks: [{ ...passed.checks[0], status: "fail" }] }),
    async () => Response.json({ ok: true, checks: [null] }),
    async () => Response.json({ ok: true, checks: [{ ...passed.checks[0], detail: 1 }] }),
  ];
  for (const fetch of cases) {
    await assert.rejects(load("lib/preflight-client.ts", {}, { fetch }).checkPackage({}), error => {
      assert.match(error.message, /draft is still here.*nothing was started/);
      assert.doesNotMatch(error.message, /PRIVATE|Server Action/);
      return true;
    });
  }
  for (const [status, expected] of [[401, /Sign in again/], [403, /permissions/], [413, /too large/]]) {
    await assert.rejects(load("lib/preflight-client.ts", {}, { fetch: async () => new Response("PRIVATE", { status }) }).checkPackage({}), expected);
  }
});
