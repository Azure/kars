// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { runInNewContext } from "node:vm";
import test from "node:test";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const { NextRequest } = require("next/server");
const { SignJWT } = require("jose");
const root = fileURLToPath(new URL("../src/", import.meta.url));
const secret = "test-only-bridge-session-signing-key";
const adminPaths = [
  "/api/operator/inference-budgets/cluster",
  "/api/operator/inference-budgets/workspaces/work",
  "/api/operator/inference-budgets/users/alice",
  "/api/operator/retention-policy",
];

function app({ sso = true, floor, roleCookie, token } = {}) {
  const env = sso ? {
    BRIDGE_OIDC_ISSUER: "https://idp.example",
    BRIDGE_OIDC_CLIENT_ID: "bridge",
    BRIDGE_OIDC_CLIENT_SECRET: "test-only-client-secret",
    BRIDGE_SESSION_SECRET: secret,
  } : {};
  if (floor !== undefined) env.BRIDGE_ROLES = floor;
  const cookies = new Map([
    ["bridge-session", token], ["bridge-role", roleCookie],
  ].filter(([, value]) => value !== undefined));
  const cache = new Map();
  const calls = [];
  function load(path) {
    if (cache.has(path)) return cache.get(path);
    const exports = {};
    cache.set(path, exports);
    const source = readFileSync(path, "utf8");
    runInNewContext(ts.transpileModule(source, {
      compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
    }).outputText, {
      exports, Headers, Response, TextEncoder, process: { env }, console,
      fetch: async (...args) => { calls.push(args); return new Response(null, { status: 204 }); },
      require: (name) => {
        if (name === "next/headers") return {
          cookies: async () => ({ get: (key) => cookies.has(key) ? { value: cookies.get(key) } : undefined }),
        };
        if (name.startsWith("@/")) return load(resolve(root, `${name.slice(2)}.ts`));
        if (name.startsWith(".")) return load(resolve(dirname(path), `${name}.ts`));
        return require(name);
      },
    });
    return exports;
  }
  function request(path, method = "PUT") {
    const headers = { cookie: [...cookies].map(([key, value]) => `${key}=${value}`).join(";") };
    return new NextRequest(`https://bridge.example${path}`, { method, headers });
  }
  return {
    edge: load(resolve(root, "proxy.ts")),
    sessions: load(resolve(root, "lib/session.ts")),
    routes: load(resolve(root, "app/api/[...path]/route.ts")),
    request, calls,
  };
}

async function token(roles, signingSecret = secret, expires = "1h", claims = {}) {
  return new SignJWT({ name: "Alice", roles, ...claims })
    .setProtectedHeader({ alg: "HS256" })
    .setSubject("alice")
    .setExpirationTime(expires)
    .sign(new TextEncoder().encode(signingSecret));
}

test("SSO admin mutations reject signed operators regardless of forged dev cookie or env floor", async () => {
  const signed = await token(["operator"]);
  for (const roleCookie of [undefined, "admin"]) {
    const service = app({ token: signed, roleCookie });
    assert.deepEqual(Array.from(await service.sessions.sessionRoles()), ["user", "operator"]);
    assert.equal(await service.sessions.canAdminister(), false);
    for (const path of adminPaths) {
      const response = await service.edge.proxy(service.request(path));
      assert.equal(response.status, 403, path);
    }
  }
});

test("SSO admin mutations never fall back to dev roles for absent, forged, expired, or malformed sessions", async () => {
  for (const signed of [
    undefined, "forged", await token(["admin"], "wrong-key"),
    await token(["admin"], secret, "0s"),
    await token([]), await token(["administrator"]),
    await token(["admin", 1]), await token(["admin"], secret, "1h", { name: {} }),
  ]) {
    const service = app({ token: signed, roleCookie: "admin", floor: "admin" });
    assert.deepEqual(Array.from(await service.sessions.sessionRoles()), []);
    const principal = await service.sessions.currentPrincipal();
    assert.equal(principal.ssoSignedIn, false);
    assert.deepEqual(Array.from(principal.roles), []);
    for (const path of adminPaths) {
      assert.equal((await service.edge.proxy(service.request(path))).status, 403, path);
      assert.equal((await service.routes.PUT(service.request(path))).status, 401, path);
    }
    assert.equal(service.calls.length, 0);
  }
});

test("genuine signed admins reach the authenticated API proxy despite a restrictive dev cookie and floor", async () => {
  const signed = await token(["admin"]);
  const service = app({ token: signed, roleCookie: "user", floor: "user" });
  assert.equal(await service.sessions.canAdminister(), true);
  assert.equal((await service.sessions.currentPrincipal()).simulated, false);
  for (const path of adminPaths) {
    const request = service.request(`${path}?source=console`);
    const edge = await service.edge.proxy(request);
    assert.equal(edge.headers.get("x-middleware-next"), "1");
    const response = await service.routes.PUT(request);
    assert.equal(response.status, 204);
    assert.equal(service.calls.at(-1)[1].headers.get("x-kars-principal-token"), signed);
    assert.equal(service.calls.at(-1)[1].headers.get("cookie"), null);
  }
});

test("operator reads and operational mutations remain available; user and auditor never become admins", async () => {
  for (const role of ["operator", "user", "auditor"]) {
    const service = app({ token: await token([role]) });
    for (const path of ["/api/operator/inference-budgets", "/api/operator/retention-policy"]) {
      assert.equal((await service.edge.proxy(service.request(path, "GET"))).headers.get("x-middleware-next"), "1");
    }
    for (const path of adminPaths) {
      for (const method of ["PUT", "POST", "PATCH", "DELETE"]) {
        assert.equal((await service.edge.proxy(service.request(path, method))).status, 403);
      }
    }
    assert.equal((await service.edge.proxy(service.request("/api/operator/inferencepolicies/example", "PATCH"))).headers.get("x-middleware-next"), "1");
  }
});

test("local development role switching is unchanged and return-to remains server-derived", async () => {
  for (const [roleCookie, floor, allowed] of [
    [undefined, undefined, true], ["operator", undefined, false],
    ["admin", "user", true], [undefined, "user", false],
  ]) {
    const service = app({ sso: false, roleCookie, floor });
    const response = await service.edge.proxy(service.request(adminPaths[0]));
    assert.equal(response.status, allowed ? 200 : 403);
  }
  const service = app({ token: await token(["user"]) });
  const request = service.request("/workspace/missions?tab=recent", "GET");
  request.headers.set("x-bridge-return-to", "https://untrusted.example");
  const response = await service.edge.proxy(request);
  assert.equal(response.headers.get("x-middleware-request-x-bridge-return-to"), "/workspace/missions?tab=recent");
});

test("non-admin browser mutations stop before the catch-all forwards a principal", async () => {
  for (const options of [{ roleCookie: "admin" }, { token: await token(["operator"]), roleCookie: "admin" }]) {
    const service = app(options);
    for (const path of adminPaths) {
      const request = service.request(path);
      const response = await service.edge.proxy(request);
      if (response.headers.get("x-middleware-next") === "1") {
        await service.routes.PUT(request);
        assert.fail("admin mutation passed the web role boundary");
      }
      assert.equal(response.status, 403);
    }
    assert.equal(service.calls.length, 0);
  }
});
