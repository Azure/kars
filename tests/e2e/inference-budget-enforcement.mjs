// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Runs only after the existing E2E harness loads its real controller/router
// images into its disposable Kind cluster. No external provider/model is used.
import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
import { prepareRouterImage } from "./kind-router-image.mjs";

const root = fileURLToPath(new URL("../../", import.meta.url));
const context = "kind-kars-e2e";
const namespace = "kars-system";
const source = "budget-provider-fixture";
const endpoint = `http://provider.${source}.svc.cluster.local:8000`;
const scratch = join(root, `.budget-kind-${process.pid}`);
const forwards = [];
let verifiedRouterReference;

function execute(binary, args, input) {
  try {
    return execFileSync(binary, args, {
      cwd: root, encoding: "utf8", input, stdio: ["pipe", "pipe", "pipe"], timeout: 120_000,
    });
  } catch {
    // Do not persist/echo argv, API bodies, signing material or bearer tokens.
    throw new Error("Disposable budget enforcement fixture command failed");
  }
}

function k(args, value) {
  return execute("kubectl", ["--context", context, "--request-timeout=30s", ...args],
    value === undefined ? undefined : JSON.stringify(value));
}
function create(value) { return JSON.parse(k(["create", "-f", "-", "-o", "json"], value)); }
function get(kind, name, ns = namespace) { return JSON.parse(k(["get", kind, name, "-n", ns, "-o", "json"])); }
const sleep = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

async function until(check, message, seconds = 120) {
  const deadline = Date.now() + seconds * 1000;
  while (Date.now() < deadline) {
    try {
      const value = await check();
      if (value) return value;
    } catch { /* The API resource may not have been materialized yet. */ }
    await sleep(500);
  }
  throw new Error(`Budget fixture deadline: ${message}`);
}

async function portForward(ns, target, port) {
  const process = spawn("kubectl", ["--context", context, "port-forward", "--address", "127.0.0.1",
    "-n", ns, target, `:${port}`], { cwd: root, stdio: ["ignore", "pipe", "pipe"] });
  forwards.push(process);
  let output = "";
  process.stdout.on("data", (data) => { output = (output + data).slice(-2048); });
  process.stderr.on("data", () => {}); // Never persist transport/API diagnostics.
  return until(() => {
    if (process.exitCode !== null) throw new Error("Port-forward exited");
    const match = output.match(/Forwarding from 127\.0\.0\.1:(\d+) ->/);
    return match && `http://127.0.0.1:${match[1]}`;
  }, "port-forward ready", 30);
}

async function request(url, path, body, timeout = 30_000) {
  const response = await fetch(`${url}${path}`, {
    method: body === undefined ? "GET" : "POST",
    headers: body === undefined ? {} : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(timeout),
  });
  const text = await response.text();
  const value = text && response.headers.get("content-type")?.includes("application/json")
    ? JSON.parse(text) : text;
  return { status: response.status, value };
}

const message = { messages: [{ role: "user", content: "fixture" }] };
function blueprint() {
  return { isolation: "standard", model: { provider: "ollama", deployment: "fixture" }, instructions: "Fixture only" };
}

function task(name, tokens, usdMicros, parent, launch) {
  return create({
    apiVersion: "kars.azure.com/v1alpha1", kind: "KarsTask", metadata: { name, namespace },
    spec: {
      objective: "Disposable governed inference accounting fixture",
      envelope: { tier: parent ? 2 : 3, authorityCeiling: parent ? 2 : 3, delegationDepth: parent ? 1 : 2,
        budget: { scope: "GovernedInference", tokens, usdMicros } },
      ...(parent ? { parentRef: { name: parent } } : {}),
      execution: { launch }, blueprint: blueprint(),
    },
  });
}

async function router(name) {
  await until(() => {
    const current = get("karstask", name);
    return current.status?.phase === "Ready" && current.status?.inferenceBudget
      && current.status?.observedGeneration === current.metadata.generation;
  }, `Task ${name} account bound`);
  const runtime = `kars-${name}`;
  const pod = await until(() => {
    const pods = JSON.parse(k(["get", "pods", "-n", runtime, "-l", `kars.azure.com/sandbox=${name}`, "-o", "json"]));
    return pods.items.find((pod) => !pod.metadata.deletionTimestamp
      && pod.status?.containerStatuses?.some((container) => container.name === "inference-router" && container.state?.running));
  }, `router container ${name}`);
  assert(pod.spec.containers.find(container => container.name === "inference-router")?.image
    === verifiedRouterReference, "Budget router must use the exact CRI-verified digest reference");
  const url = await portForward(runtime, `pod/${pod.metadata.name}`, 8443);
  await until(async () => (await request(url, "/readyz")).status === 200, `private budget readiness ${name}`);
  return { url, pod, runtime };
}

function accountFor(name) {
  const reference = get("karstask", name).status.inferenceBudget.account;
  const account = get("karsbudgetaccount", reference.name, reference.namespace);
  assert.equal(account.metadata.uid, reference.uid, "Account UID must not reset");
  return account;
}

async function scenario() {
  const nodes = execute("kind", ["get", "nodes", "--name", "kars-e2e"]).trim().split(/\s+/);
  assert(nodes.length > 0 && nodes.every((node) => node.startsWith("kars-e2e-")));
  const values = JSON.parse(execute("helm", ["get", "values", "kars", "--kube-context", context, "-n", namespace, "--all", "-o", "json"]));
  const imageProofs = [];
  const imageDirectory = join(root, "e2e-diag", "standalone");
  mkdirSync(imageDirectory, { recursive: true, mode: 0o700 });
  const image = await prepareRouterImage({
    nodes, kube: k, values,
    report: proof => {
      imageProofs.push(proof);
      console.log("BUDGET-IMAGE " + JSON.stringify(proof));
      writeFileSync(join(imageDirectory, "router-image-preflight.json"), JSON.stringify({ proofs: imageProofs }, null, 2));
    },
  });
  const digest = image.manifestDigest;
  verifiedRouterReference = image.reference;

  mkdirSync(scratch, { mode: 0o700 });
  execute("openssl", ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
    "-keyout", join(scratch, "tls.key"), "-out", join(scratch, "tls.crt"),
    "-subj", "/CN=kars-inference-budget.kars-system.svc",
    "-addext", "subjectAltName=DNS:kars-inference-budget.kars-system.svc"]);
  const certificate = readFileSync(join(scratch, "tls.crt"));
  create({ apiVersion: "v1", kind: "Secret",
    metadata: { name: "budget-fixture-tls", namespace, annotations: { "kars.azure.com/inference-budget-tls": "v1" } },
    type: "kubernetes.io/tls", data: { "tls.crt": certificate.toString("base64"),
      "tls.key": readFileSync(join(scratch, "tls.key")).toString("base64") } });
  rmSync(scratch, { recursive: true });

  create({ apiVersion: "v1", kind: "Namespace", metadata: { name: source } });
  const providerCode = `
    const http = require('node:http');
    let count = 0, hold = false, waiting = [];
    http.createServer((req, res) => {
      res.setHeader('content-type', 'application/json');
      if (req.url === '/count') return res.end(JSON.stringify({count}));
      if (req.url === '/hold') { hold = true; return res.end('{}'); }
      if (req.url === '/release') { hold = false; for (const send of waiting.splice(0)) send(); return res.end('{}'); }
      if (req.url !== '/v1/chat/completions' || req.method !== 'POST') { res.statusCode = 404; return res.end('{}'); }
      let body = '';
      req.on('data', chunk => { body += chunk; });
      req.on('end', () => {
        const request = JSON.parse(body);
        if (request.model !== 'fixture' || request.max_tokens !== 20
            || request.messages.length !== 1 || request.messages[0].content !== 'fixture') {
          res.statusCode = 400; return res.end('{}');
        }
        count++;
        const send = () => res.end(JSON.stringify({id:'fixture',object:'chat.completion',choices:[
          {index:0,message:{role:'assistant',content:'fixture'},finish_reason:'stop'}],
          usage:{prompt_tokens:3,completion_tokens:5,total_tokens:8}}));
        if (hold) waiting.push(send); else send();
      });
    }).listen(8000, '0.0.0.0');
  `;
  create({ apiVersion: "apps/v1", kind: "Deployment", metadata: { name: "provider", namespace: source },
    spec: { replicas: 1, selector: { matchLabels: { app: "budget-provider" } },
      template: { metadata: { labels: { app: "budget-provider" } }, spec: {
        containers: [{ name: "provider", image: "node:latest", command: ["node", "-e", providerCode],
          ports: [{ containerPort: 8000 }] }],
      } } } });
  create({ apiVersion: "v1", kind: "Service", metadata: { name: "provider", namespace: source },
    spec: { selector: { app: "budget-provider" }, ports: [{ port: 8000, targetPort: 8000 }] } });
  k(["rollout", "status", "-n", source, "deployment/provider", "--timeout=90s"]);
  const provider = await portForward(source, "deployment/provider", 8000);

  values.inferenceBudget = {
    enabled: true, routerImageDigest: digest, catalogVersion: "fixture-v1",
    tlsSecretName: "budget-fixture-tls", caBundle: certificate.toString("utf8"),
    nonInferenceEgressHosts: [],
    contracts: [{ id: "fixture-chat", version: "v1", validUntil: "2030-01-01T00:00:00Z",
      providerId: "ollama", endpoint, model: "fixture", operation: "ChatCompletions", outputField: "MaxTokens",
      maximumInputTokens: 10, maximumOutputTokens: 20, maximumWireBytes: 4096,
      outputBoundIncludesReasoning: true, maximumPrice: { kind: "perRequest", maximumMicros: 5 } }],
  };
  values.controller.extraEnv = (values.controller.extraEnv ?? []).filter((entry) => entry.name !== "OLLAMA_ENDPOINT");
  values.controller.extraEnv.push({ name: "OLLAMA_ENDPOINT", value: endpoint });
  values.localInference = { namespaces: [source], targets: [{ namespace: source, matchLabels: { app: "budget-provider" }, ports: [8000] }] };
  execute("helm", ["upgrade", "kars", "deploy/helm/kars", "--kube-context", context, "-n", namespace,
    "--reuse-values", "-f", "-", "--wait", "--timeout", "90s"], JSON.stringify(values));

  task("budget-money-root", 1000, 12, undefined, false);
  task("budget-money-left", 1000, 12, "budget-money-root", true);
  task("budget-money-right", 1000, 12, "budget-money-root", true);
  const left = await router("budget-money-left");
  const right = await router("budget-money-right");
  const results = await Promise.all([left, right, left].map((router) =>
    request(router.url, "/v1/chat/completions", message)));
  assert.deepEqual(results.map((result) => result.status).sort(), [200, 200, 429]);
  const money = accountFor("budget-money-left");
  assert.equal(money.metadata.uid, accountFor("budget-money-right").metadata.uid);
  assert.equal(money.status.ledger.meters.settled.usdMicros, 10);
  assert.equal(money.status.ledger.meters.settled.tokens, 16);

  task("budget-token-root", 50, 0, undefined, false);
  task("budget-token-left", 50, 0, "budget-token-root", true);
  task("budget-token-right", 50, 0, "budget-token-root", true);
  const tokenLeft = await router("budget-token-left");
  const tokenRight = await router("budget-token-right");
  const before = (await request(provider, "/count")).value.count;
  await request(provider, "/hold");
  const first = request(tokenLeft.url, "/v1/chat/completions", message, 90_000);
  await until(async () => (await request(provider, "/count")).value.count === before + 1, "actual first dispatch");
  assert.equal((await request(tokenRight.url, "/v1/chat/completions", message)).status, 429);
  assert.equal(accountFor("budget-token-left").status.ledger.meters.reserved.tokens, 30);
  await request(provider, "/release");
  assert.equal((await first).status, 200);
  assert.equal(accountFor("budget-token-left").status.ledger.meters.settled.tokens, 8);

  const count = (await request(provider, "/count")).value.count;
  assert.equal((await request(tokenRight.url, "/v1/embeddings", { input: "fixture" })).status, 503);
  assert.equal((await request(tokenRight.url, "/agents", {})).status, 403);
  assert.equal((await request(provider, "/count")).value.count, count);
  const binding = get("karstask", "budget-token-left").status.inferenceBudget;
  await request(provider, "/hold");
  const accepted = request(tokenLeft.url, "/v1/chat/completions", message, 90_000).catch(() => ({ status: 0 }));
  await until(async () => (await request(provider, "/count")).value.count === count + 1, "accepted work before cancellation");
  const current = get("karstask", "budget-token-left");
  k(["patch", "karstask", current.metadata.name, "-n", namespace, "--type=merge", "--patch-file", "-"],
    { metadata: { uid: current.metadata.uid, resourceVersion: current.metadata.resourceVersion }, spec: { execution: { launch: false } } });
  await until(() => {
    const account = get("karsbudgetaccount", binding.account.name, binding.account.namespace);
    assert.equal(account.metadata.uid, binding.account.uid);
    return account.status.ledger.meters.uncertain.tokens === 30;
  }, "cancellation conservatively accounts for accepted work");
  await request(provider, "/release");
  await accepted;
  const final = get("karsbudgetaccount", binding.account.name, binding.account.namespace);
  assert.equal(final.status.ledger.meters.settled.tokens, 8);
  assert.equal(final.status.ledger.meters.uncertain.tokens, 30);
  assert.equal(final.status.ledger.meters.reserved.tokens, 0);
  console.log("Real broker/Pod authentication, sibling token and maximum-price CAS, unsupported-route closure and accepted-work cancellation passed");
}

try {
  await scenario();
} finally {
  for (const process of forwards) {
    process.kill("SIGTERM");
  }
  rmSync(scratch, { recursive: true, force: true });
}
