import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const require = createRequire(import.meta.url);
const ts = require("typescript");

function proxy(kind, { fetch, sso = false, valid = true } = {}) {
  const source = readFileSync(new URL(`../src/app/${kind}/[...path]/route.ts`, import.meta.url), "utf8");
  const logs = [];
  const requests = [];
  const verified = [];
  const env = {
    BRIDGE_BFF_URL: "http://bff.private.svc:8081/",
    DEX_UPSTREAM_URL: "http://dex.private.svc:5556/",
  };
  const context = {
    exports: {}, Headers, Response, process: { env },
    console: { error: (...args) => logs.push(args) },
    require: (name) => {
      if (name === "@/lib/oidc-config") return { ssoConfigured: () => sso };
      if (name === "@/lib/session-token") return {
        SESSION_COOKIE: "bridge-session",
        verifySession: async (token) => { verified.push(token); return valid; },
      };
      throw new Error(`Unexpected route dependency: ${name}`);
    },
    fetch: async (...args) => {
      requests.push(args);
      return fetch ? fetch(...args) : new Response(null, { status: 204 });
    },
  };
  const output = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  runInNewContext(output, context);
  return { routes: context.exports, requests, logs, verified, env };
}

function request(kind, { method = "GET", headers = {}, token, body = null, suffix = "/items?q=one" } = {}) {
  return {
    method, body, headers: new Headers(headers),
    nextUrl: new URL(`https://bridge.example/${kind}${suffix}`),
    cookies: { get: () => token ? { value: token } : undefined },
  };
}

for (const kind of ["api", "dex"]) {
  test(`${kind}: errors expose only a stable 502 and log no credentials or upstream details`, async () => {
    const secret = "PRIVATE_QUERY_AND_ERROR";
    const upstreamError = new Error(`fetch failed at private.svc:8081 ${secret}`, {
      cause: new Error(`internal connection details ${secret}`),
    });
    const service = proxy(kind, { fetch: async () => { throw upstreamError; } });
    const response = await service.routes.GET(request(kind, { suffix: `/items?code=${secret}` }));
    assert.equal(response.status, 502);
    assert.equal(response.headers.get("content-type"), "application/json");
    assert.deepEqual(await response.json(), {
      error: { code: "bad_gateway", message: kind === "api" ? "BFF unreachable" : "OIDC IdP (Dex) unreachable" },
    });
    assert.deepEqual(service.logs, [[kind === "api"
      ? "[bridge/api] BFF upstream request failed" : "[bridge/dex] OIDC IdP upstream request failed"]]);
    assert.equal(JSON.stringify(service.logs).includes(secret), false);
  });

  test(`${kind}: all methods retain fixed upstream routing, bodies, and manual redirects`, async () => {
    const service = proxy(kind);
    for (const method of ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"]) {
      const body = new ReadableStream({ start(controller) { controller.close(); } });
      const response = await service.routes[method](request(kind, {
        method, body, suffix: "/https://untrusted.example/items?next=https://other.example",
        headers: { host: "untrusted.example", connection: "keep-alive", "x-request-id": "trace" },
      }));
      assert.equal(response.status, 204);
      const [target, init] = service.requests.at(-1);
      const expected = kind === "api" ? "http://bff.private.svc:8081" : "http://dex.private.svc:5556";
      assert.equal(target, `${expected}/${kind}/https://untrusted.example/items?next=https://other.example`);
      assert.equal(init.redirect, "manual");
      assert.equal(init.method, method);
      assert.equal(init.headers.get("host"), null);
      assert.equal(init.headers.get("connection"), null);
      assert.equal(init.headers.get("x-request-id"), "trace");
      const hasBody = method !== "GET" && method !== "HEAD";
      assert.equal(init.body, hasBody ? body : undefined);
      assert.equal(init.duplex, hasBody ? "half" : undefined);
    }
    service.env[kind === "api" ? "BRIDGE_BFF_URL" : "DEX_UPSTREAM_URL"] = "http://changed.svc:9000";
    await service.routes.GET(request(kind));
    assert.equal(service.requests.at(-1)[0], `http://changed.svc:9000/${kind}/items?q=one`);
  });
}

test("api: strips browser cookies and internal identity headers even without SSO", async () => {
  const service = proxy("api");
  await service.routes.GET(request("api", { headers: {
    cookie: "bridge-session=forged", "x-kars-principal-token": "forged",
    "x-teams-internal-secret": "forged", "x-teams-internal-signature": "forged",
    authorization: "Bearer caller-credential",
  } }));
  const headers = service.requests[0][1].headers;
  for (const name of ["cookie", "x-kars-principal-token", "x-teams-internal-secret", "x-teams-internal-signature"]) {
    assert.equal(headers.get(name), null);
  }
  assert.equal(headers.get("authorization"), "Bearer caller-credential");
});

test("api: only a verified session can supply the principal token when SSO is enabled", async () => {
  const service = proxy("api", { sso: true });
  await service.routes.GET(request("api", {
    token: "signed-session", headers: { "x-kars-principal-token": "forged" },
  }));
  assert.deepEqual(service.verified, ["signed-session"]);
  assert.equal(service.requests[0][1].headers.get("x-kars-principal-token"), "signed-session");
  for (const token of [undefined, "invalid-session"]) {
    const denied = proxy("api", { sso: true, valid: false });
    const response = await denied.routes.GET(request("api", { token }));
    assert.equal(response.status, 401);
    assert.equal(denied.requests.length, 0);
  }
});

test("api: streams SSE unchanged while removing hop-by-hop response headers", async () => {
  const upstream = new Response("data: event\n\n", {
    headers: { "content-type": "text/event-stream", connection: "keep-alive" },
  });
  const service = proxy("api", { fetch: async () => upstream });
  const response = await service.routes.GET(request("api"));
  assert.equal(response.body, upstream.body);
  assert.equal(response.headers.get("content-type"), "text/event-stream");
  assert.equal(response.headers.get("connection"), null);
  assert.equal(await response.text(), "data: event\n\n");
});

test("dex: forwards login cookies, redirects, and separate Set-Cookie headers", async () => {
  const headers = new Headers({ location: "/auth/callback?code=opaque", connection: "keep-alive" });
  headers.append("set-cookie", "csrf=one; Path=/dex; HttpOnly");
  headers.append("set-cookie", "session=two; Path=/dex; HttpOnly");
  const service = proxy("dex", {
    fetch: async () => new Response(null, { status: 302, statusText: "Found", headers }),
  });
  const response = await service.routes.GET(request("dex", { headers: { cookie: "csrf=one" } }));
  assert.equal(service.requests[0][1].headers.get("cookie"), "csrf=one");
  assert.equal(response.status, 302);
  assert.equal(response.statusText, "Found");
  assert.equal(response.headers.get("location"), "/auth/callback?code=opaque");
  assert.deepEqual(response.headers.getSetCookie(), headers.getSetCookie());
  assert.equal(response.headers.get("connection"), null);
});
