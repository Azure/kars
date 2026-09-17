// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { context, kubectl, isBudgetTokenPolicyDenial } from "./budget-api-kubectl.mjs";

function fixture(t, { stderr = "", stdout = "", status = 0 } = {}) {
  const directory = mkdtempSync(join(tmpdir(), "kars-budget-command-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const record = join(directory, "request.json");
  writeFileSync(join(directory, "kubectl"), `#!/usr/bin/env node
const fs = require("node:fs");
fs.writeFileSync(${JSON.stringify(record)}, JSON.stringify({
  args: process.argv.slice(2), input: fs.readFileSync(0, "utf8"), cwd: process.cwd()
}));
fs.writeSync(1, ${JSON.stringify(stdout)});
fs.writeSync(2, ${JSON.stringify(stderr)});
process.exit(${status});
`, { mode: 0o700 });
  const original = process.env.PATH;
  process.env.PATH = `${directory}:${original ?? ""}`;
  t.after(() => {
    if (original === undefined) delete process.env.PATH;
    else process.env.PATH = original;
  });
  const errors = [];
  t.mock.method(console, "error", (message) => errors.push(message));
  return { errors, request: () => JSON.parse(readFileSync(record, "utf8")) };
}

function commandFailed(error) {
  assert.equal(error.message, "Disposable budget API assertion command failed");
  assert.equal(error.cause, undefined);
  return true;
}

test("public CRD readiness preserves bounded diagnostics without changing context or wait arguments", (t) => {
  const diagnostic = `error: no matching resources found\n${"x".repeat(13_000)}`;
  const state = fixture(t, { stderr: diagnostic, status: 1 });
  const args = ["wait", "--for=condition=Established", "crd/karssandboxes.kars.azure.com", "--timeout=60s"];
  assert.throws(() => kubectl(args, undefined, true), commandFailed);
  assert.deepEqual(state.errors, [diagnostic.slice(0, 12_000)]);
  assert.deepEqual(state.request().args, ["--context", "kind-kars-budget-api", "--request-timeout=20s", ...args]);
  assert.equal(state.request().input, "");
});

test("Secret and TokenRequest failures never expose input, stderr or an underlying cause", (t) => {
  const secret = "fixture-private-credential";
  const state = fixture(t, { stderr: `upstream included ${secret}`, status: 1 });
  for (const args of [
    ["get", "secret", "fixture", "-o", "json"],
    ["create", "--raw", "/api/v1/namespaces/budget-api-fixture/serviceaccounts/untrusted/token", "-f", "-"],
  ]) {
    assert.throws(() => kubectl(args, { value: secret }), commandFailed);
    assert.equal(state.request().input, JSON.stringify({ value: secret }));
  }
  assert.deepEqual(state.errors, []);
});

test("synthetic authorization review can expose bounded public diagnostics without token bodies", (t) => {
  const diagnostic = "Error from server (Forbidden): synthetic fixture permission review rejected";
  const state = fixture(t, { stderr: diagnostic, status: 1 });
  const review = { apiVersion: "authorization.k8s.io/v1", kind: "SelfSubjectAccessReview",
    spec: { resourceAttributes: { namespace: "budget-api-fixture", verb: "create",
      resource: "serviceaccounts", subresource: "token", name: "untrusted" } } };
  assert.throws(() => kubectl(["create", "-f", "-"], review, true), commandFailed);
  assert.deepEqual(state.errors, [diagnostic]);
  assert(!state.request().input.includes("audiences"));
});

test("successful public and private commands preserve their output without diagnostic logging", (t) => {
  const output = '{"metadata":{"uid":"fixture-uid"}}\n';
  const state = fixture(t, { stdout: output });
  for (const publicSchema of [false, true]) {
    assert.equal(kubectl(["create", "-f", "-", "-o", "json"], { kind: "Fixture" }, publicSchema), output);
    assert.equal(state.request().input, '{"kind":"Fixture"}');
  }
  assert.deepEqual(state.errors, []);
});

test("real kubectl sends the built-in permission review without CRD discovery or relaxed validation", { timeout: 20_000 }, async (t) => {
  const directory = mkdtempSync(join(tmpdir(), "kars-budget-raw-review-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const path = "/apis/authorization.k8s.io/v1/selfsubjectaccessreviews";
  const principal = "system:serviceaccount:budget-api-fixture:untrusted";
  const review = { apiVersion: "authorization.k8s.io/v1", kind: "SelfSubjectAccessReview",
    spec: { resourceAttributes: { namespace: "budget-api-fixture", verb: "create",
      group: "", version: "v1", resource: "serviceaccounts", subresource: "token", name: "untrusted" } } };
  const requests = [];
  const server = createServer(async (req, res) => {
    let body = "";
    for await (const chunk of req) body += chunk;
    requests.push({ method: req.method, path: new URL(req.url, "http://localhost").pathname,
      actor: req.headers["impersonate-user"], body });
    res.setHeader("Content-Type", "application/json");
    if (req.method !== "POST" || new URL(req.url, "http://localhost").pathname !== path) {
      res.writeHead(403);
      res.end(JSON.stringify({ kind: "Status", status: "Failure", reason: "Forbidden", code: 403 }));
      return;
    }
    res.end(JSON.stringify({ ...review, status: { allowed: true } }));
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  t.after(() => new Promise(resolve => server.close(resolve)));
  const config = join(directory, "config.json");
  writeFileSync(config, JSON.stringify({ apiVersion: "v1", kind: "Config",
    clusters: [{ name: "fixture", cluster: { server: `http://127.0.0.1:${server.address().port}` } }],
    users: [{ name: "fixture", user: {} }],
    contexts: [{ name: context, context: { cluster: "fixture", user: "fixture" } }],
    "current-context": context }), { mode: 0o600 });
  const output = await new Promise((resolve, reject) => {
    const child = execFile("kubectl", ["--kubeconfig", config, "--context", context,
      "--request-timeout=5s", "create", "--raw", path, "-f", "-", "--as", principal],
    { encoding: "utf8", timeout: 10_000 }, (error, stdout) => error ? reject(error) : resolve(stdout));
    child.stdin.end(JSON.stringify(review));
  });
  assert.equal(JSON.parse(output).status.allowed, true);
  assert.deepEqual(requests, [{ method: "POST", path, actor: principal, body: JSON.stringify(review) }]);
});

test("private audience denial requires the exact policy and binding without exposing error contents", (t) => {
  const marker = "do-not-publish-token-material";
  const stderr = 'The serviceaccounts "untrusted" is invalid: : Invalid value: "": '
    + "ValidatingAdmissionPolicy 'kars-inference-budget-token' with binding "
    + "'kars-inference-budget-token' denied request: "
    + `Only kubelet node identities may obtain a Pod-bound governed-inference audience token\n${marker}`;
  const state = fixture(t, { stderr, status: 1 });
  assert.throws(() => kubectl(["create", "--raw", "/fixture/token"], { private: marker }), (error) => {
    commandFailed(error);
    assert.equal(isBudgetTokenPolicyDenial(error), true);
    assert(!JSON.stringify(error).includes(marker));
    assert(!error.stack.includes(marker));
    return true;
  });
  assert.deepEqual(state.errors, []);
});

for (const stderr of [
  'Error from server (Forbidden): User "untrusted" cannot create resource "serviceaccounts/token"',
  'The serviceaccounts "untrusted" is invalid: ValidatingAdmissionPolicy \'another-policy\' with binding \'kars-inference-budget-token\' denied request: Only kubelet node identities may obtain a Pod-bound governed-inference audience token',
  'The serviceaccounts "untrusted" is invalid: ValidatingAdmissionPolicy \'kars-inference-budget-token\' with binding \'another-binding\' denied request: Only kubelet node identities may obtain a Pod-bound governed-inference audience token',
  'The serviceaccounts "untrusted" is invalid: ValidatingAdmissionPolicy \'kars-inference-budget-token\' with binding \'kars-inference-budget-token\' denied request: expression resulted in an evaluation error',
  "Error from server (Forbidden): ValidatingAdmissionPolicy 'kars-inference-budget-token' with binding 'kars-inference-budget-token' denied request: Only kubelet node identities may obtain a Pod-bound governed-inference audience token",
  "Unable to connect to the server: connection refused",
]) {
  test(`non-policy failure never qualifies private-audience denial: ${stderr}`, (t) => {
    const state = fixture(t, { stderr, status: 1 });
    assert.throws(() => kubectl(["create", "--raw", "/fixture/token"], {}), (error) => {
      commandFailed(error);
      assert.equal(isBudgetTokenPolicyDenial(error), false);
      return true;
    });
    assert.deepEqual(state.errors, []);
  });
}

test("the actual preflight opts only its public CRD wait into schema diagnostics", () => {
  const source = readFileSync(new URL("./inference-budget-api.mjs", import.meta.url), "utf8");
  assert.equal(context, "kind-kars-budget-api");
  assert.ok(source.includes('import { context, kubectl, isBudgetTokenPolicyDenial } from "./budget-api-kubectl.mjs";'));
  assert.ok(source.includes("root, context, kubectl, until, namespace, controller, principal,"));
  assert.ok(source.includes('kubectl(["wait", "--for=condition=Established", `crd/${definition.metadata.name}`, "--timeout=60s"], undefined, true);'));
  assert(source.indexOf("kind: \"SelfSubjectAccessReview\"") < source.indexOf("const ordinaryToken ="));
  assert.ok(source.includes('"/apis/authorization.k8s.io/v1/selfsubjectaccessreviews", "-f", "-", "--as", principal]'));
  assert(!source.includes("--validate=false"));
  assert(source.indexOf("const ordinaryToken =") < source.indexOf("assert.throws(() => kubectl(tokenArgs, tokenRequest)"));
  assert.ok(source.includes("isBudgetTokenPolicyDenial,"));
});
