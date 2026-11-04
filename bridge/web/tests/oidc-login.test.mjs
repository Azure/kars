// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const source = ts.transpileModule(
  readFileSync(new URL("../src/app/auth/login/route.ts", import.meta.url), "utf8"),
  { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } },
).outputText;
const authExports = {};
runInNewContext(ts.transpileModule(
  readFileSync(new URL("../src/lib/auth-return.ts", import.meta.url), "utf8"),
  { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } },
).outputText, { exports: authExports, URL });

function app({ configured = true, failure } = {}) {
  const exports = {};
  const calls = [], writes = [], states = [];
  const config = { redirectUri: "https://bridge.example/auth/callback" };
  runInNewContext(source, {
    exports, URL,
    require(name) {
      if (name === "next/headers") return {
        cookies: async () => ({ set: (...args) => writes.push(args) }),
        headers: async () => new Headers({ "x-forwarded-proto": "https" }),
      };
      if (name === "@/lib/oidc-config") return { oidcConfig: () => configured ? config : null };
      if (name === "@/lib/oidc") return {
        buildAuthorizationRequest: async (...args) => {
          calls.push(args);
          if (failure) throw failure;
          return {
            url: "https://idp.example/authorize", state: "test-state",
            nonce: "test-nonce", codeVerifier: "test-verifier",
          };
        },
      };
      if (name === "@/lib/session-token") return {
        OIDC_STATE_COOKIE: "bridge-oidc-state",
        signFlowState: async value => { states.push(value); return "test-signed-flow"; },
      };
      if (name === "@/lib/auth-return") return authExports;
      return require(name);
    },
  });
  return { get: exports.GET, calls, writes, states, config };
}

test("RSC refresh and both prefetch variants cannot begin or overwrite an OIDC flow", async () => {
  const service = app();
  const document = new Request("https://bridge.example/auth/login?returnTo=%2Fconsole%2Ffleet");
  assert.equal((await service.get(document)).status, 307);
  const pending = service.writes[0];
  for (const headers of [
    { RSC: "1" },
    { rsc: "1", "next-router-prefetch": "1" },
    { rsc: "1", "next-router-segment-prefetch": "/_tree" },
  ]) {
    const response = await service.get(new Request(document.url + "&_rsc=refresh", { headers }));
    assert.equal(response.status, 401);
    assert.equal(response.headers.get("location"), null);
    assert.equal(response.headers.get("set-cookie"), null);
    assert.equal(response.headers.get("cache-control"), "private, no-store");
    assert.equal(response.headers.get("vary"), "RSC");
    assert.equal((await response.json()).error.code, "unauthorized");
  }
  assert.equal(service.calls.length, 1);
  assert.equal(service.states.length, 1);
  assert.equal(service.writes.length, 1);
  assert.equal(service.writes[0], pending);
});

test("document sign-in retains PKCE, nonce, return destination and secure cookie options", async () => {
  const service = app();
  const response = await service.get(new Request(
    "https://bridge.example/auth/login?returnTo=%2Fconsole%2Ffleet%3Ftab%3Dactive",
  ));
  assert.equal(response.status, 307);
  assert.equal(response.headers.get("location"), "https://idp.example/authorize");
  assert.equal(service.calls[0][0], service.config);
  assert.equal(service.calls[0][1], service.config.redirectUri);
  assert.deepEqual(JSON.parse(JSON.stringify(service.states)), [{
    state: "test-state", nonce: "test-nonce", codeVerifier: "test-verifier",
    redirectUri: service.config.redirectUri, returnTo: "/console/fleet?tab=active",
  }]);
  assert.deepEqual(JSON.parse(JSON.stringify(service.writes)), [[
    "bridge-oidc-state", "test-signed-flow",
    { httpOnly: true, secure: true, sameSite: "lax", path: "/", maxAge: 600 },
  ]]);
});

test("unconfigured SSO remains explicit and does not create state", async () => {
  const service = app({ configured: false });
  for (const headers of [{}, { rsc: "1" }]) {
    const response = await service.get(new Request("https://bridge.example/auth/login", { headers }));
    assert.equal(response.status, 501);
    assert.equal((await response.json()).error.code, "sso_not_configured");
  }
  assert.equal(service.calls.length, 0);
  assert.equal(service.writes.length, 0);
});

test("OIDC discovery failure propagates without state or a success-shaped redirect", async () => {
  const failure = new Error("discovery unavailable");
  const service = app({ failure });
  await assert.rejects(service.get(new Request("https://bridge.example/auth/login")), error => error === failure);
  assert.equal(service.calls.length, 1);
  assert.equal(service.states.length, 0);
  assert.equal(service.writes.length, 0);
});
