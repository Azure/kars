// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Runs only after the existing E2E harness loads its real controller/router
// images into its disposable Kind cluster. No external provider/model is used.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
import { prepareRouterImage } from "./kind-router-image.mjs";
import { PROVIDER, ENDPOINT, providerSource,
  budgetStageFacts, routerTemplateFacts, TLS_SERVER_EXTENSIONS, verifyFixtureCertificate,
  unsupportedOperationFact } from "./budget-fixture-route.mjs";
import { ownedRouterResolver, startForward, waitForOwnedRouter } from "./budget-router-readiness.mjs";

const root = fileURLToPath(new URL("../../", import.meta.url));
const context = "kind-kars-e2e";
const namespace = "kars-system";
const source = "budget-provider-fixture";
const endpoint = ENDPOINT;
const scratch = join(root, `.budget-kind-${process.pid}`);
const forwards = [];
const createdTasks = new Map();
const secrets = [];
const runtimes = new Set();
let verifiedRouterReference;

function execute(binary, args, input, deadline = Date.now() + 120_000) {
  assert(Date.now() < deadline, "Budget fixture command deadline");
  try {
    return execFileSync(binary, args, {
      cwd: root, encoding: "utf8", input, stdio: ["pipe", "pipe", "pipe"],
      timeout: Math.max(1, Math.min(120_000, deadline - Date.now())),
    });
  } catch {
    // Do not persist/echo argv, API bodies, signing material or bearer tokens.
    throw new Error("Disposable budget enforcement fixture command failed");
  }
}

function k(args, value, deadline = Date.now() + 120_000) {
  const timeout = Math.max(1, Math.min(30_000, deadline - Date.now()));
  return execute("kubectl", ["--context", context, `--request-timeout=${timeout}ms`, ...args],
    value === undefined ? undefined : JSON.stringify(value), deadline);
}
function create(value) {
  const result = JSON.parse(k(["create", "-f", "-", "-o", "json"], value));
  if (value.kind === "Secret") {
    const { name, namespace, uid } = result.metadata;
    secrets.push({ name, namespace, uid });
  }
  return result;
}
function get(kind, name, ns = namespace) { return JSON.parse(k(["get", kind, name, "-n", ns, "-o", "json"])); }
function optionalResource(kind, name, ns, deadline) {
  const output = k(["get", kind, name, ...(ns ? ["-n", ns] : []), "--ignore-not-found", "-o", "json"],
    undefined, deadline);
  return output.trim() ? JSON.parse(output) : null;
}
const sleep = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

async function until(check, message, seconds = 120) {
  const deadline = Date.now() + seconds * 1000;
  while (Date.now() < deadline) {
    const value = await check();
    if (value) return value;
    await sleep(500);
  }
  throw new Error(`Budget fixture deadline: ${message}`);
}

async function portForward(ns, target, port) {
  return (await startForward({ context, cwd: root, namespace: ns, target, port,
    deadline: Date.now() + 30_000, register: handle => forwards.push(handle) })).url;
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
  return { isolation: "standard", model: { provider: PROVIDER, deployment: "fixture" }, instructions: "Fixture only" };
}

function task(name, tokens, usdMicros, parent, launch) {
  if (launch) runtimes.add(`kars-${name}`);
  const created = create({
    apiVersion: "kars.azure.com/v1alpha1", kind: "KarsTask", metadata: { name, namespace },
    spec: {
      objective: "Disposable governed inference accounting fixture",
      envelope: { tier: parent ? 2 : 3, authorityCeiling: parent ? 2 : 3, delegationDepth: parent ? 1 : 2,
        budget: { scope: "GovernedInference", tokens, usdMicros } },
      ...(parent ? { parentRef: { name: parent } } : {}),
      execution: { launch }, blueprint: blueprint(),
    },
  });
  createdTasks.set(name, created);
  return created;
}

async function router(name) {
  const deadline = Date.now() + 120_000;
  const resolve = ownedRouterResolver({
    task: createdTasks.get(name), image: verifiedRouterReference, deadline, read: optionalResource,
    pods: (runtime, name, deadline) => JSON.parse(k(["get", "pods", "-n", runtime,
      "-l", `kars.azure.com/sandbox=${name}`, "-o", "json"], undefined, deadline)).items,
  });
  return waitForOwnedRouter({
    resolve, deadline,
    openForward: (selected, deadline) => {
      console.log("BUDGET-TEMPLATE " + JSON.stringify(selected.facts));
      return startForward({ context, cwd: root, namespace: selected.runtime,
        target: `pod/${selected.pod.metadata.name}`, port: 8443, deadline,
        register: handle => forwards.push(handle) });
    },
    probe: (url, path, timeout) => request(url, path, undefined, timeout),
    report: fact => console.log("BUDGET-READINESS " + JSON.stringify(fact)),
  });
}

function diagnostics() {
  const report = { complete: true, sources: [] };
  const targets = [
    { runtime: namespace, selector: "app.kubernetes.io/component=controller", container: "controller" },
    ...[...runtimes].map(runtime => ({ runtime, selector: "kars.azure.com/component=sandbox",
      container: "inference-router" })),
  ];
  for (const { runtime, selector, container } of targets) {
    try {
      const pods = JSON.parse(k(["get", "pods", "-n", runtime, "-l", selector, "-o", "json"]));
      for (const pod of pods.items) {
        const actualContainer = container === "controller"
          ? pod.spec.containers.find(c => c.name === "controller" || c.name === "kars-controller")?.name
          : container;
        assert(actualContainer, "Diagnostic container identity unavailable");
        const log = k(["logs", "-n", runtime, pod.metadata.name, "-c", actualContainer,
          "--tail=256", "--limit-bytes=131072"]);
        const source = { component: container, podUid: pod.metadata.uid, facts: budgetStageFacts(log) };
        if (container === "inference-router") {
          const owner = pod.metadata.ownerReferences.find(o => o.kind === "ReplicaSet" && o.controller);
          const replica = get("replicaset", owner.name, runtime);
          const deploymentOwner = replica.metadata.ownerReferences.find(o => o.kind === "Deployment" && o.controller);
          source.template = routerTemplateFacts(pod, replica, get("deployment", deploymentOwner.name, runtime));
        }
        report.sources.push(source);
      }
    } catch { report.complete = false; }
  }
  const directory = join(root, "e2e-diag", "standalone");
  mkdirSync(directory, { recursive: true, mode: 0o700 });
  writeFileSync(join(directory, "budget-readiness-stages.json"), JSON.stringify(report, null, 2));
  console.log("BUDGET-STAGES " + JSON.stringify(report));
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
    "-addext", "subjectAltName=DNS:kars-inference-budget.kars-system.svc",
    ...TLS_SERVER_EXTENSIONS]);
  const certificate = readFileSync(join(scratch, "tls.crt"));
  verifyFixtureCertificate(certificate);
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
  // Primary-model provider names select only explicitly registered endpoints.
  // OLLAMA_ENDPOINT alone does not turn informational model metadata into routing intent.
  create(providerSource(namespace));

  values.inferenceBudget = {
    enabled: true, routerImageDigest: digest, catalogVersion: "fixture-v1",
    tlsSecretName: "budget-fixture-tls", caBundle: certificate.toString("utf8"),
    nonInferenceEgressHosts: [],
    contracts: [{ id: "fixture-chat", version: "v1", validUntil: "2030-01-01T00:00:00Z",
      providerId: PROVIDER, endpoint, model: "fixture", operation: "ChatCompletions", outputField: "MaxTokens",
      maximumInputTokens: 10, maximumOutputTokens: 20, maximumWireBytes: 4096,
      outputBoundIncludesReasoning: true, maximumPrice: { kind: "perRequest", maximumMicros: 5 } }],
  };
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
  const unsupported = await request(tokenRight.url, "/v1/embeddings", { input: "fixture" });
  const denial = unsupportedOperationFact(unsupported);
  console.log("BUDGET-FAILURE-CONTRACT " + JSON.stringify(denial));
  assert.equal(unsupported.status, 503);
  assert(denial.matchesContract, "Unsupported inference must return the coded budget denial");
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
  try {
    diagnostics();
  } finally {
    try {
      const cleanup = await Promise.allSettled(forwards.map(handle => handle.stop()));
      const secretCleanup = await Promise.allSettled(secrets.map(async secret => {
        const current = optionalResource("secret", secret.name, secret.namespace);
        if (!current) return;
        assert(current.metadata.uid === secret.uid, "Fixture Secret cleanup refuses a replacement UID");
        k(["delete", "--raw", `/api/v1/namespaces/${secret.namespace}/secrets/${secret.name}`, "-f", "-"],
          { apiVersion: "v1", kind: "DeleteOptions", preconditions: { uid: secret.uid } });
      }));
      assert(secretCleanup.every(result => result.status === "fulfilled"), "UID-owned fixture Secret cleanup failed");
      assert(cleanup.every(result => result.status === "fulfilled"), "Owned fixture port-forward cleanup failed");
    } finally {
      rmSync(scratch, { recursive: true, force: true });
    }
  }
}
