// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createServer } from "node:http";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const { exportJWK, generateKeyPair, SignJWT } = require("jose");

function load(name, env = {}) {
  const exports = {};
  runInNewContext(ts.transpileModule(readFileSync(new URL(`../src/lib/${name}.ts`, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText, { exports, require, fetch, URL, URLSearchParams, Buffer, TextEncoder, process: { env } });
  return exports;
}

async function fixture(t, options = {}) {
  const calls = [];
  const { publicKey, privateKey } = await generateKeyPair("ES256");
  const jwk = { ...await exportJWK(publicKey), kid: "test-key", alg: "ES256", use: "sig" };
  const issuer = "https://bridge.example:3080/dex";
  const server = createServer(async (req, res) => {
    const chunks = [];
    for await (const chunk of req) chunks.push(chunk);
    calls.push({ path: req.url, body: Buffer.concat(chunks).toString() });
    if (req.url === options.redirectPath) {
      res.writeHead(307, { location: `${base}/must-not-follow` }).end();
      return;
    }
    res.setHeader("content-type", "application/json");
    if (req.url === "/internal/dex/.well-known/openid-configuration") {
      res.end(JSON.stringify({
        issuer: options.discoveryIssuer ?? issuer,
        authorization_endpoint: `${issuer}/auth`,
        token_endpoint: options.tokenEndpoint ?? `${issuer}/token`,
        jwks_uri: `${issuer}/keys`,
        end_session_endpoint: `${issuer}/logout`,
      }));
    } else if (req.url === "/internal/dex/token") {
      const jwt = await new SignJWT({ nonce: options.nonce ?? "expected-nonce", groups: ["kars-operators"] })
        .setProtectedHeader({ alg: "ES256", kid: "test-key" })
        .setIssuer(options.tokenIssuer ?? issuer)
        .setAudience(options.audience ?? "bridge")
        .setSubject("beta-user")
        .setExpirationTime(options.expires ?? "1h")
        .sign(privateKey);
      res.end(JSON.stringify({ id_token: jwt }));
    } else if (req.url === "/internal/dex/keys") {
      res.end(JSON.stringify({ keys: [jwk] }));
    } else {
      res.writeHead(404).end("{}");
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  t.after(() => new Promise((resolve) => { server.close(resolve); server.closeAllConnections(); }));
  const base = `http://127.0.0.1:${server.address().port}`;
  const cfg = {
    issuer, backchannelIssuer: `${base}/internal/dex`, clientId: "bridge",
    clientSecret: "test-only-client-secret", scopes: ["openid", "groups"],
    roleClaim: "groups", roleMap: { "kars-operators": "operator" },
  };
  const oidc = load("oidc");
  return { oidc, cfg, calls, exchange: () => oidc.exchangeCodeForIdentity(
    cfg, "test-code", "https://bridge.example:3080/auth/callback", "test-verifier", "expected-nonce",
  ) };
}

test("internal discovery/token/JWKS preserve the public issuer, browser URLs and verified roles", async (t) => {
  const { oidc, cfg, calls, exchange } = await fixture(t);
  const auth = await oidc.buildAuthorizationRequest(cfg, "https://bridge.example:3080/auth/callback");
  const url = new URL(auth.url);
  assert.equal(`${url.origin}${url.pathname}`, `${cfg.issuer}/auth`);
  assert.equal(url.searchParams.get("code_challenge_method"), "S256");
  assert.ok(auth.nonce && auth.state && auth.codeVerifier);
  const identity = await exchange();
  assert.equal(identity.sub, "beta-user");
  assert.deepEqual(Array.from(oidc.rolesFromClaims(cfg, identity.claims)), ["operator"]);
  assert.equal(new URL(await oidc.endSessionUrl(cfg, "https://bridge.example:3080/")).pathname, "/dex/logout");
  assert.deepEqual(calls.map((call) => call.path), [
    "/internal/dex/.well-known/openid-configuration", "/internal/dex/token", "/internal/dex/keys",
  ]);
  const body = new URLSearchParams(calls[1].body);
  assert.equal(body.get("code_verifier"), "test-verifier");
  assert.equal(body.get("client_secret"), "test-only-client-secret");
});

test("discovery cannot substitute a different issuer", async (t) => {
  const { exchange, calls } = await fixture(t, { discoveryIssuer: "https://wrong.example/dex" });
  await assert.rejects(exchange(), /discovery issuer/);
  assert.equal(calls.length, 1);
});

test("backchannel mapping rejects token endpoints outside the configured issuer", async (t) => {
  for (const tokenEndpoint of ["https://elsewhere.example/dex/token", "https://bridge.example:3080/dex-evil/token"]) {
    const { exchange, calls } = await fixture(t, { tokenEndpoint });
    await assert.rejects(exchange(), /beneath the configured issuer/);
    assert.equal(calls.length, 1);
  }
});

test("discovery and credential-bearing token requests never follow redirects", async (t) => {
  for (const redirectPath of ["/internal/dex/.well-known/openid-configuration", "/internal/dex/token"]) {
    const { exchange, calls } = await fixture(t, { redirectPath });
    await assert.rejects(exchange());
    assert.ok(!calls.some((call) => call.path === "/must-not-follow"));
  }
});

test("backchannel does not weaken issuer, audience, expiry or nonce verification", async (t) => {
  for (const options of [
    { tokenIssuer: "http://internal/dex" }, { audience: "different-client" },
    { expires: "-1h" }, { nonce: "replayed-nonce" },
  ]) {
    const { exchange } = await fixture(t, options);
    await assert.rejects(exchange());
  }
});

test("generic OIDC defaults to public endpoints; internal base is explicit", () => {
  const env = {
    BRIDGE_OIDC_ISSUER: "https://issuer.example/", BRIDGE_OIDC_CLIENT_ID: "bridge",
    BRIDGE_OIDC_CLIENT_SECRET: "test-secret", BRIDGE_SESSION_SECRET: "test-session-secret",
  };
  assert.equal(load("oidc-config", env).oidcConfig().backchannelIssuer, null);
  env.BRIDGE_OIDC_BACKCHANNEL_ISSUER = " http://dex:5556/dex/ ";
  assert.equal(load("oidc-config", env).oidcConfig().backchannelIssuer, "http://dex:5556/dex");
});
