// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { generateKeyPairSync } from "node:crypto";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { beforeEach, test } from "node:test";
import { fileURLToPath } from "node:url";
import { runInNewContext } from "node:vm";
import * as jose from "jose";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const root = fileURLToPath(new URL("../src/", import.meta.url));
const cfg = { sessionSecret: "test-only-github-manifest-signing-secret-not-a-credential" };
const pem = generateKeyPairSync("rsa", { modulusLength: 2048 }).privateKey.export({ type: "pkcs8", format: "pem" });
const origin = "http://localhost:3181";
const code = "a".repeat(40);
let env;
let helper;
let sessions;
class BffError extends Error { constructor(code) { super("PRIVATE upstream response"); this.code = code; } }

function load(file) {
  const exports = {};
  const resolve = name => {
    if (name === "jose") return jose;
    if (name === "./oidc-config") return { oidcConfig: () => env.sso ? cfg : null };
    if (name === "./config") return { ALL_ROLES: ["admin", "operator", "user", "auditor"] };
    if (name === "@/lib/github-app-registration") return helper;
    if (name === "@/lib/session-token") return sessions;
    if (name === "@/lib/session") return { canAdminister: async () => env.admin, hasRole: async role => role === "operator" && env.operator };
    if (name === "next/headers") return {
      cookies: async () => ({ get: name => env.cookies.get(name), set: (name, value, options) => {
        env.writes.push({ name, value, options });
        if (options.maxAge === 0) env.cookies.delete(name); else env.cookies.set(name, { value });
      } }),
      headers: async () => env.headers,
    };
    if (name === "next/cache") return { revalidatePath: path => env.revalidated.push(path) };
    if (name === "@/lib/bff") return {
      BffError,
      getGithubApp: async () => { env.reads++; if (env.readError) throw new Error("PRIVATE read failure"); return env.app; },
      putGithubApp: async credentials => { env.puts.push(credentials); return { configured: true, name: "Test App", note: "Stored." }; },
      deleteGithubApp: async () => { env.deletes++; },
      convertGithubAppManifest: async value => { env.conversions.push(value); if (env.conversionError) throw env.conversionError; return { configured: true }; },
    };
    return require(name);
  };
  const compiled = ts.transpileModule(readFileSync(`${root}${file}`, "utf8"), {
    fileName: file, compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
    reportDiagnostics: true,
  });
  assert.equal(compiled.diagnostics?.length ?? 0, 0);
  runInNewContext(compiled.outputText, { exports, require: resolve, Headers, Request, Response, URL, FormData, File, TextEncoder, Error });
  return exports;
}
helper = load("lib/github-app-registration.ts");
sessions = load("lib/session-token.ts");
const actions = load("app/console/configuration/github-app-actions.ts");
const callback = load("app/console/configuration/github-app/callback/route.ts");

beforeEach(async () => {
  env = { sso: true, admin: true, operator: true, cookies: new Map(), writes: [], reads: 0, puts: [], conversions: [], revalidated: [],
    app: { configured: false, storage_ready: true }, headers: new Headers({ host: "localhost:3181", origin }) };
  env.session = await sessions.signSession({ sub: "admin-a", name: "Admin", roles: ["admin"] });
  env.cookies.set(sessions.SESSION_COOKIE, { value: env.session });
});
function registrationForm(overrides = {}) {
  const form = new FormData();
  for (const [key, value] of Object.entries({ name: "Kars test", owner: "personal", visibility: "private", ...overrides })) form.set(key, value);
  return form;
}
function uploadForm(value = pem) {
  const form = new FormData();
  form.set("app_id", "12345");
  form.set("private_key_file", new File([value], "github.pem"));
  return form;
}
async function start(form = registrationForm()) {
  return actions.startGithubAppRegistrationAction({ error: null, registration: null }, form);
}
async function callbackRequest(options = {}) {
  const prepared = await start();
  assert.ok(prepared.registration);
  const state = new URL(prepared.registration.url).searchParams.get("state");
  const url = new URL(`${origin}${helper.GITHUB_APP_CALLBACK}`);
  url.searchParams.set("state", options.state ?? state);
  if (!options.noCode) url.searchParams.set("code", options.code ?? code);
  if (options.duplicate) url.searchParams.append(options.duplicate, "duplicate");
  if (options.error) url.searchParams.set("error", options.error);
  return new Request(url, { headers: { host: "localhost:3181", ...options.headers } });
}

test("upload parses RSA file server-side and returns no key contents", async () => {
  const result = await actions.putGithubAppAction({}, uploadForm());
  assert.equal(env.puts.length, 1);
  assert.equal(env.puts[0].private_key, pem.trim());
  assert.equal(env.puts[0].app_id, "12345");
  assert.match(result.ok, /Verified against GitHub/);
  assert.doesNotMatch(JSON.stringify(result), /PRIVATE KEY|PRIVATE upstream/);
});

for (const [label, mutate] of [
  ["missing file", form => form.delete("private_key_file")],
  ["plaintext field", form => { form.delete("private_key_file"); form.set("private_key", pem); }],
  ["empty file", form => form.set("private_key_file", new File([], "empty.pem"))],
  ["oversized file", form => form.set("private_key_file", new File(["x".repeat(16385)], "large.pem"))],
  ["invalid PEM", form => form.set("private_key_file", new File(["PRIVATE invalid contents"], "bad.pem"))],
  ["non-numeric ID", form => form.set("app_id", "<script>")],
]) test(`upload rejects ${label} without network or exposing contents`, async () => {
  const form = uploadForm(); mutate(form);
  const result = await actions.putGithubAppAction({}, form);
  assert.ok(result.error); assert.equal(env.puts.length, 0); assert.equal(env.reads, 0);
  assert.doesNotMatch(result.error, /PRIVATE invalid|BEGIN|<script\b/i);
});

test("upload preserves operator access but denies users and configured replacement", async () => {
  env.admin = false;
  assert.ok((await actions.putGithubAppAction({}, uploadForm())).ok);
  env.operator = false;
  assert.match((await actions.putGithubAppAction({}, uploadForm())).error, /Operator/);
  env.operator = true; env.app.configured = true;
  assert.match((await actions.putGithubAppAction({}, uploadForm())).error, /already configured/);
  assert.equal(env.puts.length, 1);
});

test("upload rejects EC keys, encrypted keys and unexpected file read errors safely", async () => {
  const ec = generateKeyPairSync("ec", { namedCurve: "prime256v1" }).privateKey.export({ type: "pkcs8", format: "pem" });
  assert.match((await actions.putGithubAppAction({}, uploadForm(ec))).error, /RSA/);
  assert.match((await actions.putGithubAppAction({}, uploadForm("-----BEGIN ENCRYPTED PRIVATE KEY-----\nPRIVATE\n-----END ENCRYPTED PRIVATE KEY-----"))).error, /RSA/);
  const form = uploadForm();
  form.get("private_key_file").text = async () => { throw new Error("PRIVATE file failure"); };
  assert.equal((await actions.putGithubAppAction({}, form)).error, "Could not read the private-key file.");
  assert.equal(env.puts.length, 0);
});

test("guided setup prepares explicit private manifest without creating an App or installation", async () => {
  const result = await start();
  assert.equal(new URL(result.registration.url).origin, "https://github.com");
  const manifest = JSON.parse(result.registration.manifest);
  assert.equal(manifest.public, false);
  assert.equal(manifest.redirect_url, `${origin}${helper.GITHUB_APP_CALLBACK}`);
  assert.equal(manifest.hook_attributes.active, false);
  assert.equal(manifest.request_oauth_on_install, false);
  assert.equal(manifest.default_permissions.contents, "write");
  assert.equal(manifest.default_permissions.actions, "read");
  assert.equal(manifest.default_permissions.secret_scanning_alerts, undefined);
  assert.equal(env.conversions.length, 0); assert.equal(env.puts.length, 0);
  assert.equal(env.writes[0].options.httpOnly, true);
  assert.equal(env.writes[0].options.sameSite, "lax");
  assert.equal(env.writes[0].options.maxAge, 3600);
  assert.equal(env.writes[0].options.secure, false);
  assert.doesNotMatch(result.registration.manifest, /PRIVATE KEY|sessionSecret|client_secret/);
});

test("organization/public/security-signal options are explicit", async () => {
  const result = await start(registrationForm({ owner: "organization", organization: "test-org", visibility: "public", security_signals: "on" }));
  assert.equal(new URL(result.registration.url).pathname, "/organizations/test-org/settings/apps/new");
  const manifest = JSON.parse(result.registration.manifest);
  assert.equal(manifest.public, true);
  assert.equal(manifest.default_permissions.vulnerability_alerts, "read");
  assert.equal(manifest.default_permissions.security_events, "read");
  assert.equal(manifest.default_permissions.secret_scanning_alerts, "read");
});

for (const overrides of [{ owner: "invalid" }, { visibility: "invalid" }, { name: "<script>" }, { owner: "organization", organization: "a/../../evil" }, { owner: "organization", organization: "https://github.com/org" }]) {
  test(`invalid manifest input is rejected: ${JSON.stringify(overrides)}`, async () => {
    assert.ok((await start(registrationForm(overrides))).error);
    assert.equal(env.reads, 0); assert.equal(env.writes.length, 0);
  });
}

for (const scenario of ["non-admin", "no SSO", "invalid session", "no storage", "configured", "unavailable storage", "cross-origin", "public HTTP"]) {
  test(`registration refuses ${scenario} before issuing flow`, async () => {
    if (scenario === "non-admin") env.admin = false;
    if (scenario === "no SSO") env.sso = false;
    if (scenario === "invalid session") env.cookies.set(sessions.SESSION_COOKIE, { value: "invalid" });
    if (scenario === "no storage") env.app.storage_ready = false;
    if (scenario === "configured") env.app.configured = true;
    if (scenario === "unavailable storage") env.readError = true;
    if (scenario === "cross-origin") env.headers.set("origin", "https://evil.example");
    if (scenario === "public HTTP") env.headers = new Headers({ host: "bridge.example", origin: "http://bridge.example" });
    const result = await start();
    assert.ok(result.error); assert.equal(result.registration, null); assert.equal(env.writes.length, 0);
    assert.doesNotMatch(result.error, /PRIVATE/);
  });
}

test("TLS origin is preserved and its flow cookie is Secure", async () => {
  env.headers = new Headers({ host: "pod:3000", "x-forwarded-host": "bridge.example", "x-forwarded-proto": "https", origin: "https://bridge.example" });
  const result = await start();
  assert.equal(JSON.parse(result.registration.manifest).redirect_url, `https://bridge.example${helper.GITHUB_APP_CALLBACK}`);
  assert.equal(env.writes[0].options.secure, true);
});

test("signed flow is bound to exact session, nonce, origin, purpose and expiry", async () => {
  const flow = await helper.createGithubAppFlow(env.session, origin);
  assert.equal(await helper.verifyGithubAppFlow(flow.token, env.session, flow.state, origin), true);
  for (const [token, session, state, requestOrigin] of [
    [flow.token + "bad", env.session, flow.state, origin],
    [flow.token, "different-session", flow.state, origin],
    [flow.token, env.session, "b".repeat(43), origin],
    [flow.token, env.session, flow.state, "https://other.example"],
    [env.session, env.session, flow.state, origin],
  ]) assert.equal(await helper.verifyGithubAppFlow(token, session, state, requestOrigin), false);
  const payload = jose.decodeJwt(flow.token);
  for (const overrides of [{ exp: 1 }, { aud: "oidc" }, { iss: "other" }, { iat: 1 }]) {
    const changed = await new jose.SignJWT({ ...payload, ...overrides }).setProtectedHeader({ alg: "HS256" }).sign(new TextEncoder().encode(cfg.sessionSecret));
    assert.equal(await helper.verifyGithubAppFlow(changed, env.session, flow.state, origin), false);
  }
});

test("callback converts exactly once, clears flow and redirects without code or secrets", async () => {
  const request = await callbackRequest();
  const response = await callback.GET(request);
  assert.equal(response.status, 303);
  assert.equal(response.headers.get("location"), "/console/configuration?githubApp=connected");
  assert.equal(response.headers.get("referrer-policy"), "no-referrer");
  assert.deepEqual(env.conversions, [code]);
  assert.deepEqual(env.revalidated, ["/console/configuration"]);
  assert.equal(env.writes.at(-1).options.maxAge, 0);
  assert.equal(env.writes.at(-1).options.path, helper.GITHUB_APP_FLOW_PATH);
  assert.match((await callback.GET(request)).headers.get("location"), /invalid-flow/);
  assert.equal(env.conversions.length, 1);
});

for (const options of [{ state: "bad" }, { code: "../secret" }, { duplicate: "code" }, { duplicate: "state" }, { noCode: true }, { error: "PRIVATE failure" }, { headers: { host: "evil.example", "x-forwarded-proto": "https" } }]) {
  test(`callback refuses invalid/cancelled flow ${JSON.stringify(options)}`, async () => {
    const response = await callback.GET(await callbackRequest(options));
    assert.equal(response.status, 303); assert.equal(env.conversions.length, 0);
    assert.match(response.headers.get("location"), /githubApp=(invalid-flow|cancelled)$/);
    assert.doesNotMatch(response.headers.get("location"), /PRIVATE|secret/);
  });
}

for (const headers of [{ rsc: "1" }, { "next-router-prefetch": "1" }, { purpose: "prefetch" }, { "sec-purpose": "prefetch" }]) {
  test(`prefetch cannot consume callback ${JSON.stringify(headers)}`, async () => {
    const request = await callbackRequest({ headers });
    assert.equal((await callback.GET(request)).status, 204);
    assert.equal(env.conversions.length, 0); assert.equal(env.writes.length, 1);
  });
}

test("callback rechecks current authorization and signed session", async () => {
  const request = await callbackRequest();
  env.admin = false;
  assert.match((await callback.GET(request)).headers.get("location"), /unauthorized/);
  env.admin = true; env.cookies.set(sessions.SESSION_COOKIE, { value: "invalid" });
  assert.match((await callback.GET(request)).headers.get("location"), /session-expired/);
  assert.equal(env.conversions.length, 0);
});

test("conversion failure is sanitized and not retried; already-configured is distinct", async () => {
  for (const error of [new Error("PRIVATE pem client-secret"), new BffError("conflict")]) {
    const request = await callbackRequest(); env.conversionError = error;
    const response = await callback.GET(request);
    assert.match(response.headers.get("location"), error instanceof BffError ? /configuration-changed/ : /conversion-failed/);
    assert.doesNotMatch(response.headers.get("location"), /PRIVATE|pem|client-secret/);
    const count = env.conversions.length;
    await callback.GET(request);
    assert.equal(env.conversions.length, count);
  }
});
