// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { execFileSync } from "node:child_process";
import { createHash, randomUUID, X509Certificate } from "node:crypto";
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { applyReviewedGrant, credentialGrantsCommand } from "../commands/credential-grants.js";
import { continuityFixture, privateAuthoritySnapshot } from "./private-activation-fixtures.js";
import { PRIVATE_PREFIX as P, canonical, digest, type Execute } from "./private-activation.js";
import { verifyRetiredSealedRoot } from "./private-activation-continuity.js";
import { reviewLateScope, verifyRetiredLateScope } from "./private-activation-late-scope.js";
import { PrivateCommandFailure } from "./private-activation-command-diagnostics.js";

const HISTORY = `${P}root-retirement`;
const VERSION = "kars.azure.com/services-credential-version";
const ADMIN = "router-services-admin";
const SOURCE = "kars.azure.com/sandbox-uid";
const NS = "kars.azure.com/namespace-uid";
const consumer = "kars-late/Deployment/late";
const AUTHORIZATION = `sha256:${"a".repeat(64)}`;
const SNAPSHOT_MARKER = "KARS_PRIVATE_LATE_SANDBOX_CHECKS ";
const cliProcess = vi.hoisted(() => ({ execute: vi.fn() }));
vi.mock("execa", () => ({ execa: cliProcess.execute }));

const tlsDirectory = path.join(process.cwd(), `.private-late-tls-${randomUUID()}`);
const tlsIdentities: { caPem: string; certificatePem: string; privateKeyPem: string; expiresAt: number }[] = [];
beforeAll(() => {
  mkdirSync(tlsDirectory);
  const openssl = (...args: string[]) => execFileSync("openssl", args, { cwd: tlsDirectory, stdio: "pipe" });
  writeFileSync(path.join(tlsDirectory, "leaf.ext"), "basicConstraints=critical,CA:FALSE\nsubjectAltName=DNS:control-sandbox.kars.internal\nextendedKeyUsage=serverAuth\n");
  for (let index = 0; index < 2; index++) {
    openssl("req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes",
      "-keyout", "ca.key", "-out", "ca.pem", "-days", "2", "-subj", "/CN=Kars test CA", "-addext", "basicConstraints=critical,CA:TRUE");
    openssl("req", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes", "-keyout", "leaf.key",
      "-out", "leaf.csr", "-subj", "/CN=Kars test control");
    openssl("x509", "-req", "-in", "leaf.csr", "-CA", "ca.pem", "-CAkey", "ca.key", "-set_serial", String(index + 1),
      "-days", "1", "-extfile", "leaf.ext", "-out", "leaf.pem");
    const pem = (name: string) => readFileSync(path.join(tlsDirectory, name), "utf8");
    const certificatePem = pem("leaf.pem");
    tlsIdentities.push({ caPem: pem("ca.pem"), certificatePem, privateKeyPem: pem("leaf.key"),
      expiresAt: Date.parse(new X509Certificate(certificatePem).validTo) / 1000 });
  }
});
afterAll(() => { rmSync(tlsDirectory, { recursive: true, force: true }); });

async function setup(suspended: boolean | null = null, tls = false, memory = false) {
  const f = continuityFixture();
  await applyReviewedGrant(f.execute, await f.document());
  await applyReviewedGrant(f.execute, await f.document("second"));
  const namespace: any = { kind: "Namespace", metadata: {
    name: "kars-late", uid: "runtime-ns", resourceVersion: "1", annotations: {
      "kars.azure.com/namespace-claim-version": "v1", "kars.azure.com/sandbox-name": "late",
      "kars.azure.com/sandbox-namespace": "work", [SOURCE]: "sandbox",
    } } };
  const task: any = { apiVersion: "kars.azure.com/v1alpha1", kind: "KarsTask", metadata: {
    name: "task", namespace: "work", uid: "task-uid", resourceVersion: "1", generation: 1,
  }, spec: { execution: { launch: true }, objective: "Retain real Task authority" },
  status: { phase: "Ready", sandboxRef: { name: "late" }, observedGeneration: 1,
    envelopeDigest: AUTHORIZATION, conditions: [{ type: "Ready", status: "True" }] } };
  const sandbox: any = { apiVersion: "kars.azure.com/v1alpha1", kind: "KarsSandbox", metadata: {
    name: "late", namespace: "work", uid: "sandbox", resourceVersion: "1", generation: 1,
    annotations: { [NS]: "runtime-ns" }, ownerReferences: [{
      apiVersion: "kars.azure.com/v1alpha1", kind: "KarsTask", name: "task", uid: "task-uid", controller: true,
    }] }, spec: { credentialsRef: { name: "kars-credential-bundle-source", uid: "bundle-uid" },
    ...(suspended === null ? {} : { suspended }) }, status: { phase: "Running", observedGeneration: 1,
      conditions: [{ type: "Ready", status: "True", observedGeneration: 1 }] } };
  const secret: any = { kind: "Secret", type: "Opaque", metadata: {
    name: ADMIN, namespace: "kars-late", uid: "admin-secret", resourceVersion: "1",
    labels: { "app.kubernetes.io/managed-by": "kars-controller" },
    annotations: { [SOURCE]: "sandbox", [NS]: "runtime-ns" },
  }, data: { "control-token": Buffer.from("A".repeat(64)).toString("base64") } };
  const deployment: any = { apiVersion: "apps/v1", kind: "Deployment", metadata: {
    name: "late", namespace: "kars-late", uid: "late-deployment", resourceVersion: "1", generation: 1,
    labels: { "kars.azure.com/sandbox": "late", "kars.azure.com/component": "sandbox" },
    annotations: { "kars.azure.com/credential-sandbox-uid": "sandbox", "kars.azure.com/credential-namespace-uid": "runtime-ns" },
  }, spec: { replicas: suspended ? 0 : 1, selector: { matchLabels: { app: "late" } }, template: {
    metadata: { labels: { app: "late" }, annotations: { [VERSION]: "admin-secret:1" } },
    spec: { automountServiceAccountToken: false, volumes: [
      { name: "governed-services-control", secret: { secretName: ADMIN, items: [{ key: "control-token", path: "control-token" }] } },
      { name: "optional-app", secret: { secretName: "router-github-app", optional: true } },
    ], containers: [{ name: "inference-router", image: "fixture", env: [
      { name: "KARS_SERVICE_IDENTITY_JSON", value: JSON.stringify({
        task: { uid: "task-uid", namespace: "work", name: "task" },
        task_authorization: AUTHORIZATION, task_generation: 1,
      }) },
    ], volumeMounts: [{ name: "governed-services-control", mountPath: "/etc/kars/services", readOnly: true }] }] } } },
  status: { observedGeneration: 1, updatedReplicas: suspended ? 0 : 1, availableReplicas: suspended ? 0 : 1 } };
  const tlsConfiguration = (index: number) => ({ ...tlsIdentities[index], serverName: "control-sandbox.kars.internal",
    identity: JSON.parse(deployment.spec.template.spec.containers[0].env[0].value) });
  const setTls = (configuration: unknown) => { secret.data["config.json"] = Buffer.from(JSON.stringify(configuration)).toString("base64"); };
  if (tls) {
    const identity = JSON.parse(deployment.spec.template.spec.containers[0].env[0].value);
    deployment.spec.template.spec.containers[0].env[0].value = JSON.stringify({ ...identity, managed: true,
      sandbox: { uid: "sandbox", namespace: "work", name: "late" }, namespace_uid: "runtime-ns" });
    deployment.spec.template.spec.volumes[0].secret.items.push({ key: "config.json", path: "config.json" });
    setTls(tlsConfiguration(0));
  }
  const rawHash = (value: string | Buffer) => createHash("sha256").update(value).digest("hex");
  const account = `kars-memory-${rawHash("sandbox").slice(0, 40)}`;
  const anchor: any = { kind: "Secret", metadata: { name: account, namespace: "kars-memory-system",
    uid: "memory-anchor-0", resourceVersion: "1", ownerReferences: [{ apiVersion: "v1", kind: "ServiceAccount",
      name: account, uid: "memory-sa", controller: true, blockOwnerDeletion: false }] } };
  const memoryAccount: any = { kind: "ServiceAccount", automountServiceAccountToken: false, metadata: {
    name: account, namespace: "kars-memory-system", uid: "memory-sa", resourceVersion: "1",
    annotations: { "kars.azure.com/memory-sandbox-uid": "sandbox" }, ownerReferences: [{ apiVersion: "v1",
      kind: "Namespace", name: "kars-memory-system", uid: "memory-ns", controller: true, blockOwnerDeletion: false }] } };
  const memoryConfiguration = (index: number) => {
    const expires = Math.floor(Date.now() / 1000) + 3600;
    const claims = { sub: `system:serviceaccount:kars-memory-system:${account}`, exp: expires,
      "kubernetes.io": { namespace: "kars-memory-system", serviceaccount: { name: account, uid: "memory-sa" },
        secret: { name: account, uid: `memory-anchor-${index}` } } };
    const part = (value: unknown) => Buffer.from(JSON.stringify(value)).toString("base64url");
    return { identity: { namespace: "work", namespace_uid: f.namespace("work").metadata.uid, owner_kind: "KarsTask",
      owner_name: "task", owner_uid: "task-uid", principal: "principal" }, map_uid: "retained-memory-map",
    anchor_uid: `memory-anchor-${index}`, epoch: rawHash(Buffer.from(secret.data["control-token"], "base64")),
    api_url: "https://10.96.0.1:443", ca: tlsIdentities[0]!.caPem, sandbox_uid: "sandbox", expires_at: expires,
    token: `${part({ alg: "RS256", typ: "JWT" })}.${part(claims)}.${part(`test-signature-${index}`)}` };
  };
  const setMemory = (configuration: unknown) => { secret.data["builtin-memory.json"] = Buffer.from(JSON.stringify(configuration)).toString("base64"); };
  const setVersion = () => {
    const { "builtin-memory.json": _memory, ...data } = secret.data;
    deployment.spec.template.metadata.annotations[VERSION] = memory
      ? `admin-secret:control-v1:${digest(data)}` : `admin-secret:${secret.metadata.resourceVersion}`;
  };
  if (memory) {
    deployment.spec.template.spec.volumes.push({ name: "builtin-memory", secret: { secretName: ADMIN,
      items: [{ key: "builtin-memory.json", path: "builtin-memory.json" }] } });
    deployment.spec.template.spec.containers[0].volumeMounts.push({ name: "builtin-memory", mountPath: "/etc/kars/builtin-memory", readOnly: true });
    f.objects.set(f.key("secret", account, "kars-memory-system"), anchor);
    f.objects.set(f.key("serviceaccount", account, "kars-memory-system"), memoryAccount);
    f.objects.set(f.key("namespace", "kars-memory-system"), { kind: "Namespace", metadata: {
      name: "kars-memory-system", uid: "memory-ns", resourceVersion: "1", labels: { "kars.azure.com/component": "private-memory" } } });
    setMemory(memoryConfiguration(0));
    setVersion();
  }
  const projection = { kind: "Secret", metadata: { name: "projection", uid: "projection-uid", resourceVersion: "8" },
    data: { CUSTOMER_KEY: "preserved-value" } };
  const source = { kind: "Secret", metadata: { name: "source", uid: "source-uid", resourceVersion: "9" },
    data: { CUSTOMER_KEY: "preserved-source" } };
  for (const [kind, object, ns] of [
    ["namespace", namespace, ""], ["karstask", task, "work"], ["karssandbox", sandbox, "work"],
    ["deployments.apps", deployment, "kars-late"], ["secret", secret, "kars-late"],
    ["secret", projection, "kars-late"], ["secret", source, "work"],
  ] as const) f.objects.set(f.key(kind, object.metadata.name, ns), object);
  const pod = (uid: string) => {
    f.objects.set(f.key("replicasets.apps", "late-rs", "kars-late"), {
      kind: "ReplicaSet", metadata: { name: "late-rs", namespace: "kars-late", uid: "late-rs-uid", resourceVersion: "1",
        ownerReferences: [{ apiVersion: "apps/v1", kind: "Deployment", name: "late", uid: "late-deployment", controller: true }] },
      spec: { template: structuredClone(deployment.spec.template) },
    });
    return { kind: "Pod", metadata: { name: uid, namespace: "kars-late", uid, resourceVersion: "1",
      annotations: structuredClone(deployment.spec.template.metadata.annotations),
      ownerReferences: [{ apiVersion: "apps/v1", kind: "ReplicaSet", name: "late-rs", uid: "late-rs-uid", controller: true }] },
    spec: structuredClone(deployment.spec.template.spec) };
  };
  f.pods.set("kars-late", suspended ? [] : [pod("old-running"), {
    ...pod("old-terminating"), metadata: { ...pod("old-terminating").metadata, deletionTimestamp: "2026-09-12T01:00:00Z" },
  }]);
  let rotate = true;
  let rotateTls = true;
  let rotateMemoryAnchor = true;
  let keepPods = false;
  const updateDeployment = () => {
    deployment.metadata.resourceVersion = String(Number(deployment.metadata.resourceVersion) + 1);
    deployment.metadata.generation++;
    deployment.status = { observedGeneration: deployment.metadata.generation,
      updatedReplicas: deployment.spec.replicas, availableReplicas: deployment.spec.replicas };
  };
  const execute: Execute = async (args, input) => {
    const result = await f.execute(args, input);
    if (args[0] !== "patch") return result;
    const patch = JSON.parse(args[args.indexOf("-p") + 1]!);
    if (args[1] === "karssandbox" && patch.spec) {
      sandbox.metadata.generation++;
      sandbox.status.observedGeneration = sandbox.metadata.generation;
      if (patch.spec.suspended === null) delete sandbox.spec.suspended;
      if (sandbox.spec.suspended !== true) {
        deployment.spec.replicas = 1;
        updateDeployment();
        f.pods.set("kars-late", [pod("new-current")]);
      }
    }
    if (args[1] === "deployments.apps" && patch.spec?.replicas === 0 && !keepPods) f.pods.set("kars-late", []);
    if (args[1] === "namespace" && args[2] === "kars-late"
      && JSON.parse(namespace.metadata.annotations[HISTORY]).phase === "Rotating") {
      expect(sandbox.spec.suspended).toBe(true);
      expect(f.pods.get("kars-late")).toEqual([]);
      expect(deployment.spec.replicas).toBe(0);
      if (rotate) secret.data["control-token"] = Buffer.from("B".repeat(64)).toString("base64");
      if (tls && rotateTls) setTls(tlsConfiguration(1));
      if (memory) {
        if (rotateMemoryAnchor) anchor.metadata.uid = "memory-anchor-1";
        setMemory(memoryConfiguration(rotateMemoryAnchor ? 1 : 0));
      }
      secret.metadata.resourceVersion = "2";
      secret.metadata.annotations[`${P}epoch`] = namespace.metadata.annotations[`${P}epoch`];
      deployment.spec.template.metadata.annotations[`${P}epoch`] = namespace.metadata.annotations[`${P}epoch`];
      setVersion();
      updateDeployment();
    }
    return result;
  };
  const document = (run = execute) => f.document("work", [consumer], run);
  const preserved = () => structuredClone({ root: f.namespace("core"), reader: privateAuthoritySnapshot(f.namespace("reader")),
    otherGrant: f.grant("second"), otherAuthority: f.authority.get(f.grant("second").metadata.uid),
    rootDeployment: f.deployment, rootPods: f.pods.get("core"), source, projection, task });
  f.calls.length = 0;
  return { ...f, namespace, task, sandbox, secret, deployment, document, execute, preserved, setTls, tlsConfiguration,
    setMemory, memoryConfiguration, anchor, memoryAccount, setVersion,
    refuseMemoryAnchorRotation: () => { rotateMemoryAnchor = false; },
    refuseTlsRotation: () => { rotateTls = false; },
    refuseRotation: () => { rotate = false; }, keepPods: () => { keepPods = true; } };
}

describe("reviewed late runtime private enrollment", () => {
  beforeEach(() => {
    cliProcess.execute.mockReset();
    vi.spyOn(console, "error").mockImplementation(() => {});
  });
  afterEach(() => { vi.restoreAllMocks(); });

  it("retires real Task token and TLS material before restoring the same owned execution", async () => {
    const f = await setup(null, true);
    const preserved = f.preserved();
    await applyReviewedGrant(f.execute, await f.document());
    const receipt = JSON.parse(f.namespace.metadata.annotations[HISTORY]);
    expect(receipt.phase).toBe("Qualified");
    expect(receipt.baseline.tls.key).not.toBe(receipt.qualified.material.tls.key);
    expect(receipt.baseline.key).not.toBe(receipt.qualified.material.key);
    expect(receipt.baseline.object.uid).toBe(receipt.qualified.material.object.uid);
    expect(JSON.stringify(receipt)).not.toMatch(/BEGIN|privateKeyPem|certificatePem/);
    expect(f.preserved()).toEqual(preserved);
    expect(f.sandbox.spec.suspended).toBeUndefined();
  });

  it("retires TLS and built-in memory bound authority with the maintained control-v1 version", async () => {
    const f = await setup(null, true, true);
    const preserved = f.preserved();
    const document = await f.document();
    expect(f.calls.filter(args => args[0] === "get" && args[1] === "secret").every(args =>
      args.includes("jsonpath-as-json={.metadata}"))).toBe(true);
    await applyReviewedGrant(f.execute, document);
    const receipt = JSON.parse(f.namespace.metadata.annotations[HISTORY]);
    expect(receipt.phase).toBe("Qualified");
    expect(receipt.baseline.memory.anchor).toBe("memory-anchor-0");
    expect(receipt.qualified.material.memory.anchor).toBe("memory-anchor-1");
    expect(receipt.qualified.material.memory.binding).toBe(receipt.baseline.memory.binding);
    expect(receipt.qualified.material.memory.epoch).not.toBe(receipt.baseline.memory.epoch);
    expect(receipt.qualified.material.memory.key).not.toBe(receipt.baseline.memory.key);
    expect(receipt.qualified.material.memory.version).toBe(f.deployment.spec.template.metadata.annotations[VERSION]);
    expect(JSON.stringify(receipt)).not.toMatch(/BEGIN|test-signature|expires_at|kubernetes.io/);
    expect(f.preserved()).toEqual(preserved);
    await f.document();
  });

  it("does not treat a new memory token on the old bound anchor as retirement", async () => {
    const f = await setup(null, true, true);
    f.refuseMemoryAnchorRotation();
    await expect(applyReviewedGrant(f.execute, await f.document())).rejects.toThrow(/not rotated/);
    expect(f.sandbox.spec.suspended).toBe(true);
    expect(JSON.parse(f.namespace.metadata.annotations[HISTORY]).phase).toBe("Rotating");
  });

  it("waits for controller memory reissuance without qualifying the old epoch", async () => {
    const f = await setup(null, true, true);
    const old = f.secret.data["builtin-memory.json"];
    let next: string | undefined;
    let waited = false;
    const execute: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (args[0] === "patch" && args[1] === "namespace" && args[2] === "kars-late"
        && JSON.parse(f.namespace.metadata.annotations[HISTORY]).phase === "Rotating") {
        next = f.secret.data["builtin-memory.json"];
        f.secret.data["builtin-memory.json"] = old;
      } else if (next && args[0] === "get" && args[1] === "secret" && args[2] === ADMIN && args.includes("json")) {
        f.secret.data["builtin-memory.json"] = next;
        next = undefined;
        waited = true;
      }
      return result;
    };
    await applyReviewedGrant(execute, await f.document(execute));
    expect(waited).toBe(true);
    expect(JSON.parse(f.namespace.metadata.annotations[HISTORY]).phase).toBe("Qualified");
  });

  it("does not qualify a changed retained memory binding during rotation", async () => {
    const f = await setup(null, true, true);
    const execute: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (args[0] === "patch" && args[1] === "namespace" && args[2] === "kars-late"
        && JSON.parse(f.namespace.metadata.annotations[HISTORY]).phase === "Rotating") {
        f.setMemory({ ...f.memoryConfiguration(1), map_uid: "replacement-memory" });
      }
      return result;
    };
    await expect(applyReviewedGrant(execute, await f.document(execute))).rejects.toThrow(/not rotated/);
    expect(f.sandbox.spec.suspended).toBe(true);
  });

  it.each(["identity", "anchor", "token", "account", "owner", "http", "CA", "expiry", "unknown-key", "version"])(
    "rejects invalid built-in memory authority before retirement: %s", async fault => {
      const f = await setup(null, true, true);
      const configuration: any = f.memoryConfiguration(0);
      if (fault === "identity") configuration.identity.owner_uid = "foreign";
      if (fault === "anchor") f.anchor.metadata.uid = "replacement";
      if (fault === "token") configuration.token = "not.a.token";
      if (fault === "account") f.memoryAccount.automountServiceAccountToken = true;
      if (fault === "owner") f.memoryAccount.metadata.ownerReferences[0].uid = "foreign";
      if (fault === "http") configuration.api_url = "http://10.96.0.1:443";
      if (fault === "CA") configuration.ca += configuration.ca;
      if (fault === "expiry") configuration.expires_at = 1;
      if (fault === "unknown-key") configuration.extra = "PRIVATE-DO-NOT-EMIT";
      f.setMemory(configuration);
      if (fault === "version") f.secret.data["control-token"] = Buffer.from("C".repeat(64)).toString("base64");
      await expect(applyReviewedGrant(f.execute, await f.document())).rejects.toThrow(/memory configuration|current control epoch|safely/);
      expect(f.sandbox.spec.suspended).toBeUndefined();
      expect(f.namespace.metadata.annotations[HISTORY]).toBeUndefined();
      expect(JSON.stringify(f.calls)).not.toContain("PRIVATE-DO-NOT-EMIT");
    });

  it.each(["extra-item", "optional", "agent-mount", "writable", "duplicate"])(
    "rejects customized memory mounts in metadata-only preview: %s", async fault => {
      const f = await setup(null, true, true);
      const spec = f.deployment.spec.template.spec;
      const volume = spec.volumes.find((v: any) => v.name === "builtin-memory");
      const router = spec.containers[0];
      const mount = router.volumeMounts.find((v: any) => v.name === "builtin-memory");
      if (fault === "extra-item") volume.secret.items.push({ key: "other", path: "other" });
      if (fault === "optional") volume.secret.optional = true;
      if (fault === "agent-mount") spec.containers.push({ name: "agent", volumeMounts: [structuredClone(mount)] });
      if (fault === "writable") mount.readOnly = false;
      if (fault === "duplicate") spec.volumes.push(structuredClone(volume));
      f.pods.set("kars-late", []);
      await expect(f.document()).rejects.toThrow();
      expect(f.sandbox.spec.suspended).toBeUndefined();
    });

  it.each(["token", "TLS"])("does not qualify an unchanged %s key using epoch stamps", async unchanged => {
    const f = await setup(null, true);
    if (unchanged === "token") f.refuseRotation(); else f.refuseTlsRotation();
    await expect(applyReviewedGrant(f.execute, await f.document())).rejects.toThrow(/not rotated/);
    expect(f.sandbox.spec.suspended).toBe(true);
    expect(JSON.parse(f.namespace.metadata.annotations[HISTORY]).phase).toBe("Rotating");
  });

  it.each(["identity", "hostname", "private-key", "CA", "expiry", "extra-field", "malformed", "missing-config", "extra-key"])(
    "rejects mismatched Task TLS material without suspending execution: %s", async fault => {
      const f = await setup(null, true);
      const configuration: any = f.tlsConfiguration(0);
      if (fault === "identity") configuration.identity.task.uid = "foreign-task";
      if (fault === "hostname") configuration.serverName = "foreign.kars.internal";
      if (fault === "private-key") configuration.privateKeyPem = tlsIdentities[1]!.privateKeyPem;
      if (fault === "CA") configuration.caPem = tlsIdentities[1]!.caPem;
      if (fault === "expiry") configuration.expiresAt = 1;
      if (fault === "extra-field") configuration.url = "https://foreign.invalid";
      f.setTls(configuration);
      if (fault === "malformed") f.secret.data["config.json"] = "not-base64";
      if (fault === "missing-config") delete f.secret.data["config.json"];
      if (fault === "extra-key") f.secret.data.extra = "unexpected";
      await expect(applyReviewedGrant(f.execute, await f.document())).rejects.toThrow(/TLS configuration|credential keys/);
      expect(f.sandbox.spec.suspended).toBeUndefined();
      expect(f.namespace.metadata.annotations[HISTORY]).toBeUndefined();
    });

  it("rejects TLS bound to another Sandbox even when the Deployment and config agree", async () => {
    const f = await setup(true, true);
    const configuration = f.tlsConfiguration(0);
    configuration.identity.sandbox.uid = "foreign";
    f.deployment.spec.template.spec.containers[0].env[0].value = JSON.stringify(configuration.identity);
    f.setTls(configuration);
    await expect(applyReviewedGrant(f.execute, await f.document())).rejects.toThrow(/TLS configuration/);
    expect(f.sandbox.spec.suspended).toBe(true);
    expect(f.namespace.metadata.annotations[HISTORY]).toBeUndefined();
  });

  it("rejects TLS configuration on a token-only runtime rather than ignoring extra authority", async () => {
    const f = await setup();
    f.setTls(f.tlsConfiguration(0));
    await expect(applyReviewedGrant(f.execute, await f.document())).rejects.toThrow(/credential keys/);
    expect(f.sandbox.spec.suspended).toBeUndefined();
  });

  it("rejects pre-retirement TLS changes even when the token is unchanged", async () => {
    const f = await setup(null, true);
    const execute: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (args[0] === "patch" && args[1] === "deployments.apps") f.setTls(f.tlsConfiguration(1));
      return result;
    };
    await expect(applyReviewedGrant(execute, await f.document(execute))).rejects.toThrow(/before retirement/);
    expect(f.sandbox.spec.suspended).toBe(true);
    expect(JSON.parse(f.namespace.metadata.annotations[HISTORY]).phase).toBe("Pausing");
  });

  it.each(["retired", "running", "pods", "template", "secret", "namespace", "pending"])(
    "checks the full late-enrollment proof for migration-retired execution: %s", async fault => {
      const f = await setup();
      await applyReviewedGrant(f.execute, await f.document());
      const activation = f.grant("work").spec.privateActivation;
      const scope = activation.namespaces.find((value: any) => value.namespace.name === "kars-late");
      const root = digest(JSON.parse(f.objects.get(f.key("namespace", "core")).metadata.annotations[HISTORY]));
      f.deployment.spec.replicas = 0;
      f.pods.set("kars-late", []);
      if (fault === "running") f.deployment.spec.replicas = 1;
      if (fault === "pods") f.pods.set("kars-late", [{ metadata: { name: "retained", uid: "retained", resourceVersion: "1" } }]);
      if (fault === "template") f.deployment.spec.template.spec.containers[0].image = "unreviewed";
      if (fault === "secret") f.secret.metadata.resourceVersion = "replaced";
      if (fault === "namespace") f.namespace.metadata.uid = "replaced";
      if (fault === "pending") {
        const proof = JSON.parse(f.namespace.metadata.annotations[HISTORY]);
        proof.phase = "Restoring";
        f.namespace.metadata.annotations[HISTORY] = canonical(proof);
      }
      if (fault === "retired") {
        await verifyRetiredLateScope(f.execute, activation, scope, root);
        await expect(reviewLateScope(f.execute, activation, scope, root)).rejects.toThrow();
        f.objects.get(f.key("deployment", "kars-controller", "core")).spec.replicas = 0;
        f.objects.set(f.key("deployment", "late", "kars-late"), f.deployment);
        f.pods.set("core", []);
        expect(activation.namespaces[0].namespace.name).not.toBe("core");
        await verifyRetiredSealedRoot(f.execute, activation, [f.deployment.metadata.uid]);
      } else await expect(verifyRetiredLateScope(f.execute, activation, scope, root)).rejects.toThrow();
    },
  );

  it.each(["uid", "spec", "task", "template", "namespace", "root", "private-key", "unchanged-version", "deadline"])(
    "does not retry a pause conflict after %s changes or lacks a fresh version", async fault => {
      const f = await setup();
      const review = await f.document();
      let attempts = 0;
      const run: Execute = async (args, input) => {
        if (args[0] === "patch" && args[1] === "karssandbox"
          && JSON.parse(args[args.indexOf("-p") + 1]!).spec.suspended === true) {
          attempts++;
          if (fault !== "unchanged-version") f.sandbox.metadata.resourceVersion = "fresh-version";
          if (fault === "uid") f.sandbox.metadata.uid = "replacement";
          if (fault === "spec") f.sandbox.spec.unreviewed = true;
          if (fault === "task") f.task.status.envelopeDigest = `sha256:${"b".repeat(64)}`;
          if (fault === "template") f.deployment.spec.template.spec.containers[0].image = "unreviewed";
          if (fault === "namespace") f.namespace.metadata.annotations[`${P}state`] = "Qualified";
          if (fault === "root") f.objects.get(f.key("namespace", "core")).metadata.annotations[HISTORY] = "{}";
          if (fault === "private-key") f.secret.data["control-token"] = Buffer.from("C".repeat(64)).toString("base64");
          if (fault === "deadline") vi.spyOn(Date, "now").mockReturnValue(Date.now() + 121_000);
          throw Object.assign(new Error("private-command-canary"), {
            exitCode: 1, stderr: "Error from server (Conflict): private-object-canary",
          });
        }
        return f.execute(args, input);
      };
      const failure = await applyReviewedGrant(run, review).then(() => undefined, error => error);
      expect(failure).toBeInstanceOf(Error);
      expect(failure).not.toBeInstanceOf(TypeError);
      expect(failure.message + JSON.stringify(failure)).not.toContain("canary");
      expect(attempts).toBe(1);
      expect(f.namespace.metadata.annotations[`${P}state`]).toBe(fault === "namespace" ? "Qualified" : "Pending");
      expect(f.sandbox.spec.suspended).toBeUndefined();
      expect(f.grant().spec.writers).toEqual([]);
    });

  it.each(["Forbidden", "Timeout", "Unknown"])("does not retry an ambiguous or %s pause error", async reason => {
    const f = await setup();
    const review = await f.document();
    let attempts = 0;
    const run: Execute = async (args, input) => {
      if (args[0] === "patch" && args[1] === "karssandbox"
        && JSON.parse(args[args.indexOf("-p") + 1]!).spec.suspended === true) {
        attempts++;
        throw Object.assign(new Error("private-command-canary"), {
          exitCode: 1, stderr: reason === "Unknown" ? "private-error-canary" : `Error from server (${reason}): private-error-canary`,
        });
      }
      return f.execute(args, input);
    };
    const failure = await applyReviewedGrant(run, review).then(() => undefined, error => error);
    expect(failure).toBeInstanceOf(PrivateCommandFailure);
    expect(failure.facts.serverReason).toBe(reason);
    expect(failure.message).not.toContain("canary");
    expect(attempts).toBe(1);
  });

  it("bounds repeated known pause conflicts and preserves the last sanitized failure", async () => {
    const f = await setup();
    const review = await f.document();
    const versions: string[] = [];
    const run: Execute = async (args, input) => {
      if (args[0] === "patch" && args[1] === "karssandbox"
        && JSON.parse(args[args.indexOf("-p") + 1]!).spec.suspended === true) {
        const patch = JSON.parse(args[args.indexOf("-p") + 1]!);
        expect(patch.metadata.uid).toBe(f.sandbox.metadata.uid);
        expect(patch.metadata.resourceVersion).toBe(f.sandbox.metadata.resourceVersion);
        versions.push(patch.metadata.resourceVersion);
        f.sandbox.metadata.resourceVersion = `${versions.length + 1}`;
        throw Object.assign(new Error("private-command-canary"), {
          exitCode: 1, stderr: "Error from server (Conflict): private-object-canary",
        });
      }
      return f.execute(args, input);
    };
    const failure = await applyReviewedGrant(run, review).then(() => undefined, error => error);
    expect(failure).toBeInstanceOf(PrivateCommandFailure);
    expect(failure.facts).toMatchObject({ phase: "Pausing", operation: "patch", resourceKind: "KarsSandbox", serverReason: "Conflict" });
    expect(versions).toHaveLength(3);
    expect(new Set(versions).size).toBe(3);
    expect(f.namespace.metadata.annotations[`${P}state`]).toBe("Pending");
    expect(f.sandbox.spec.suspended).toBeUndefined();
    expect(f.grant().spec.writers).toEqual([]);
  });

  it("recognizes an already-applied reviewed pause without issuing another suspend update", async () => {
    const f = await setup();
    const review = await f.document();
    let attempts = 0;
    const before = f.preserved();
    const run: Execute = async (args, input) => {
      if (args[0] === "patch" && args[1] === "karssandbox"
        && JSON.parse(args[args.indexOf("-p") + 1]!).spec.suspended === true) {
        attempts++;
        await f.execute(args, input);
        throw Object.assign(new Error("concurrent reviewed pause"), {
          exitCode: 1, stderr: "Error from server (Conflict): changed version",
        });
      }
      return f.execute(args, input);
    };
    await applyReviewedGrant(run, review);
    expect(attempts).toBe(1);
    expect(f.namespace.metadata.annotations[`${P}state`]).toBe("Qualified");
    expect(f.preserved()).toEqual(before);
    expect(f.sandbox.spec.suspended).toBeUndefined();
    expect(f.deployment.spec.replicas).toBe(1);
  });

  it("does not issue another pause if revalidation consumes the remaining deadline", async () => {
    const f = await setup();
    const review = await f.document();
    let attempts = 0;
    let expired = false;
    const run: Execute = async (args, input) => {
      if (args[0] === "patch" && args[1] === "karssandbox") {
        attempts++;
        f.sandbox.metadata.resourceVersion = "fresh-version";
        throw Object.assign(new Error("conflict"), {
          exitCode: 1, stderr: "Error from server (Conflict): changed version",
        });
      }
      const result = await f.execute(args, input);
      if (attempts && !expired && args[0] === "get" && args[1] === "deployment" && args[2] === "kars-controller") {
        expired = true;
        vi.spyOn(Date, "now").mockReturnValue(Date.now() + 121_000);
      }
      return result;
    };
    await expect(applyReviewedGrant(run, review)).rejects.toBeInstanceOf(PrivateCommandFailure);
    expect(expired).toBe(true);
    expect(attempts).toBe(1);
    expect(f.sandbox.spec.suspended).toBeUndefined();
  });

  it("sanitizes actual registered command process failures before exposing the exception", async () => {
    cliProcess.execute.mockRejectedValue(Object.assign(new Error("private-argv-canary"), {
      exitCode: 1, stderr: "Error from server (Forbidden): private-secret-canary", stdout: "private-data-canary",
    }));
    const result = await credentialGrantsCommand().parseAsync([
      "preview", "--namespace", "private-name-canary", "--writer", "reader/bff",
    ], { from: "user" }).then(() => undefined, error => error);
    expect(result).toBeInstanceOf(PrivateCommandFailure);
    expect(result.facts).toEqual({ version: 1, phase: "Unscoped", operation: "get",
      resourceKind: "Namespace", serverReason: "Forbidden", exitCode: 1 });
    expect(result.message + JSON.stringify(result)).not.toContain("canary");
  });

  it("accepts an identical current Sandbox after an intervening status PATCH advances only resourceVersion", async () => {
    const f = await setup();
    let updated = false;
    const statusUpdate: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (!updated && args[0] === "get" && args[1] === "karstask") {
        updated = true;
        f.sandbox.status = structuredClone(f.sandbox.status);
        f.sandbox.metadata.resourceVersion = "2";
      }
      return result;
    };
    const before = f.preserved();
    const review = await f.document(statusUpdate);
    expect(updated).toBe(true);
    expect(f.calls.every(args => args[0] === "get")).toBe(true);
    expect(f.calls.filter(args => args[0] === "get" && args[1] === "karssandbox")).toHaveLength(3);
    expect(f.calls.filter(args => args[0] === "get" && args[1] === "karstask")).toHaveLength(2);
    expect(vi.mocked(console.error).mock.calls.some(([value]) => String(value).startsWith(SNAPSHOT_MARKER))).toBe(false);
    await applyReviewedGrant(f.execute, review);
    expect(f.preserved()).toEqual(before);
    expect(f.sandbox.spec.suspended).toBeUndefined();
  });

  it.each(["same-status", "changed-spec", "stale-ready"])(
    "exercises the shipped --observe preview and validation command with concurrent %s", async fault => {
      const f = await setup();
      const output = vi.spyOn(console, "log").mockImplementation(() => {});
      let updated = false;
      cliProcess.execute.mockImplementation(async (program: string, args: string[], options: { input?: string }) => {
        expect(program).toBe("kubectl");
        const stdout = await f.execute(args, options.input);
        if (!updated && args[0] === "get" && args[1] === "karstask") {
          updated = true;
          f.sandbox.metadata.resourceVersion = "2";
          if (fault === "same-status") f.sandbox.status = structuredClone(f.sandbox.status);
          if (fault === "changed-spec") f.sandbox.spec.credentialsRef.uid = "unreviewed-source";
          if (fault === "stale-ready") f.sandbox.status.conditions[0].observedGeneration = 0;
        }
        return { stdout };
      });
      const preview = credentialGrantsCommand().parseAsync([
        "preview", "--namespace", "work", "--writer", "reader/bff", "--observe", "late",
        "--private-root", "core", "--private-controller-profile", "kcm-certificate",
        "--private-consumer", consumer,
      ], { from: "user" });
      if (fault === "same-status") {
        await preview;
        expect(output).toHaveBeenCalledTimes(1);
        const review = JSON.parse(String(output.mock.calls[0]![0]));
        expect(review.spec.observationTargets).toEqual([{ kind: "KarsSandbox", namespace: "work", name: "late", uid: "sandbox" }]);
        expect(review.spec.privateActivation.phase).toBe("reviewed");
        expect(JSON.stringify(review)).not.toContain("control-token");
        expect(JSON.stringify(review)).not.toContain("conditions");
      } else {
        await expect(preview).rejects.toThrow();
        expect(output).not.toHaveBeenCalled();
      }
      expect(updated).toBe(true);
      expect(f.calls.every(args => ["get", "auth"].includes(args[0]!))).toBe(true);
      expect(f.namespace.metadata.annotations[HISTORY]).toBeUndefined();
    });

  it.each(["uid", "spec", "generation", "owner", "labels", "annotations", "finalizers", "managed-fields",
    "namespace-binding", "observation-authority", "status-content"])(
    "does not treat concurrent Sandbox %s drift as a harmless resourceVersion update", async fault => {
      const f = await setup();
      let changed = false;
      const race: Execute = async (args, input) => {
        const result = await f.execute(args, input);
        if (!changed && args[0] === "get" && args[1] === "karstask") {
          changed = true;
          f.sandbox.metadata.resourceVersion = "2";
          if (fault === "uid") f.sandbox.metadata.uid = "must-not-be-logged";
          if (fault === "spec") f.sandbox.spec.credentialsRef.uid = "changed-source";
          if (fault === "generation") f.sandbox.metadata.generation = 2;
          if (fault === "owner") f.sandbox.metadata.ownerReferences[0].uid = "changed-task";
          if (fault === "labels") f.sandbox.metadata.labels = { changed: "must-not-be-logged" };
          if (fault === "annotations") f.sandbox.metadata.annotations.unreviewed = "must-not-be-logged";
          if (fault === "finalizers") f.sandbox.metadata.finalizers = ["changed-finalizer"];
          if (fault === "managed-fields") f.sandbox.metadata.managedFields = [{ manager: "changed-manager" }];
          if (fault === "namespace-binding") f.sandbox.metadata.annotations[NS] = "changed-namespace";
          if (fault === "observation-authority") f.sandbox.status.serviceObservation = { phase: "Prepared" };
          if (fault === "status-content") f.sandbox.status.conditions[0].message = "changed-status";
        }
        return result;
      };
      await expect(f.document(race)).rejects.toThrow();
      expect(changed).toBe(true);
      expect(f.calls.every(args => args[0] === "get")).toBe(true);
      const markers = vi.mocked(console.error).mock.calls.map(([value]) => String(value))
        .filter(value => value.startsWith(SNAPSHOT_MARKER));
      expect(markers).toEqual([`${SNAPSHOT_MARKER}{"resourceVersionMatch":false,"observedGenerationMatch":true,"phaseRunningMatch":true,"readyConditionMatch":true}`]);
      expect(markers.join("")).not.toContain("must-not-be-logged");
      expect(f.namespace.metadata.annotations[HISTORY]).toBeUndefined();
    });

  it.each(["observed-generation", "phase", "missing-ready", "false-ready", "stale-ready", "missing-ready-generation", "malformed-conditions"])(
    "reports only fixed readiness match booleans for %s without a fallback", async fault => {
      const f = await setup();
      if (fault === "observed-generation") f.sandbox.status.observedGeneration = 0;
      if (fault === "phase") f.sandbox.status.phase = "Degraded";
      if (fault === "missing-ready") f.sandbox.status.conditions = [];
      if (fault === "false-ready") f.sandbox.status.conditions[0].status = "False";
      if (fault === "stale-ready") f.sandbox.status.conditions[0].observedGeneration = 0;
      if (fault === "missing-ready-generation") delete f.sandbox.status.conditions[0].observedGeneration;
      if (fault === "malformed-conditions") f.sandbox.status.conditions = {};
      await expect(f.document()).rejects.toThrow();
      const markers = vi.mocked(console.error).mock.calls.map(([value]) => String(value))
        .filter(value => value.startsWith(SNAPSHOT_MARKER));
      expect(markers).toHaveLength(1);
      expect(JSON.parse(markers[0]!.slice(SNAPSHOT_MARKER.length))).toEqual({
        resourceVersionMatch: true, observedGenerationMatch: fault !== "observed-generation",
        phaseRunningMatch: fault !== "phase",
        readyConditionMatch: ["observed-generation", "phase"].includes(fault),
      });
      expect(f.calls.every(args => args[0] === "get")).toBe(true);
    });

  it.each(["task-spec", "task-owner", "task-uid", "task-authorization", "task-metadata",
    "deployment-env", "deployment-metadata", "namespace"])(
    "rechecks the bounded read set and preserves authority on concurrent %s drift", async fault => {
      const f = await setup(true);
      let changed = false;
      const race: Execute = async (args, input) => {
        const result = await f.execute(args, input);
        if (!changed && args[0] === "get" && args[1] === "karstask") {
          changed = true;
          if (fault === "task-spec") f.task.spec.objective = "changed";
          if (fault === "task-owner") f.task.metadata.ownerReferences = [{ uid: "changed-team" }];
          if (fault === "task-uid") f.task.metadata.uid = "changed-task";
          if (fault === "task-authorization") f.task.status.envelopeDigest = `sha256:${"b".repeat(64)}`;
          if (fault === "task-metadata") f.task.metadata.labels = { changed: "authority" };
          if (fault === "deployment-env") f.deployment.spec.template.spec.containers[0].env[0].value = "{}";
          if (fault === "deployment-metadata") f.deployment.metadata.labels.changed = "authority";
          if (fault === "namespace") f.namespace.metadata.annotations[SOURCE] = "changed-sandbox";
        }
        return result;
      };
      await expect(f.document(race)).rejects.toThrow();
      expect(changed).toBe(true);
      expect(f.calls.every(args => args[0] === "get")).toBe(true);
      expect(f.namespace.metadata.annotations[HISTORY]).toBeUndefined();
      expect(f.sandbox.spec.suspended).toBe(true);
    });

  it("accepts an identical Task status snapshot without rebasing its authority", async () => {
    const f = await setup();
    let updated = false;
    const race: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (!updated && args[0] === "get" && args[1] === "karstask") {
        updated = true;
        f.task.metadata.resourceVersion = "2";
      }
      return result;
    };
    const review = await f.document(race);
    expect(updated).toBe(true);
    expect(f.calls.every(args => args[0] === "get")).toBe(true);
    await applyReviewedGrant(f.execute, review);
    expect(JSON.parse(f.namespace.metadata.annotations[HISTORY]).runtime.task.authorization).toBe(AUTHORIZATION);
  });

  it("does not promote a first-read stale Ready condition when the second read becomes current", async () => {
    const f = await setup();
    f.sandbox.status.conditions[0].observedGeneration = 0;
    const race: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (args[0] === "get" && args[1] === "karstask") {
        f.sandbox.status.conditions[0].observedGeneration = 1;
        f.sandbox.metadata.resourceVersion = "2";
      }
      return result;
    };
    await expect(f.document(race)).rejects.toThrow();
    expect(console.error).toHaveBeenCalledWith(`${SNAPSHOT_MARKER}{"resourceVersionMatch":false,"observedGenerationMatch":true,"phaseRunningMatch":true,"readyConditionMatch":true}`);
    expect(f.calls.every(args => args[0] === "get")).toBe(true);
  });

  it("revalidates the same snapshot constraints during apply before any mutation", async () => {
    const f = await setup();
    const review = await f.document();
    f.calls.length = 0;
    let changed = false;
    const race: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (!changed && args[0] === "get" && args[1] === "karstask") {
        changed = true;
        f.sandbox.metadata.resourceVersion = "2";
        f.sandbox.spec.credentialsRef.uid = "changed-before-apply";
      }
      return result;
    };
    await expect(applyReviewedGrant(race, review)).rejects.toThrow();
    expect(changed).toBe(true);
    expect(f.calls.every(args => ["get", "auth"].includes(args[0]!))).toBe(true);
    expect(f.namespace.metadata.annotations[HISTORY]).toBeUndefined();
  });

  it("still rejects changed shared-root authority after preview before any mutation", async () => {
    const f = await setup();
    const review = await f.document();
    f.objects.get(f.key("deployment", "kars-controller", "core")).spec.template.spec.containers[0].image = "changed-root";
    f.calls.length = 0;
    await expect(applyReviewedGrant(f.execute, review)).rejects.toThrow();
    expect(f.calls.every(args => ["get", "auth"].includes(args[0]!))).toBe(true);
    expect(f.namespace.metadata.annotations[HISTORY]).toBeUndefined();
  });

  it.each([null, false, true])("retires real owned Pod UIDs, verifies token rotation and restores suspension %s without touching shared authority", async original => {
    const f = await setup(original);
    const before = f.preserved();
    const review = await f.document();
    expect(console.error).toHaveBeenCalledWith(expect.stringContaining("controller admin-key rotation"));
    expect(f.calls.every(args => args[0] === "get")).toBe(true);
    expect(f.calls.filter(args => args[1] === "secret").every(args => args.includes("jsonpath-as-json={.metadata}"))).toBe(true);
    const oldKey = f.secret.data["control-token"];
    await applyReviewedGrant(f.execute, review);
    expect(f.preserved()).toEqual(before);
    expect(f.secret.metadata.uid).toBe("admin-secret");
    expect(f.secret.data["control-token"]).not.toBe(oldKey);
    expect(f.sandbox.metadata.uid).toBe("sandbox");
    expect(f.sandbox.spec.suspended ?? null).toBe(original);
    expect(f.deployment.metadata.uid).toBe("late-deployment");
    expect(f.deployment.spec.replicas).toBe(original ? 0 : 1);
    const state = JSON.parse(f.namespace.metadata.annotations[HISTORY]);
    expect(state.version).toBe(4);
    expect(state.phase).toBe("Qualified");
    expect(state.runtime.task.authorization).toBe(AUTHORIZATION);
    expect(state.captured).toEqual(original ? [] : ["old-running", "old-terminating"]);
    expect(state.baseline.key).not.toBe(state.qualified.material.key);
    expect(f.calls.some(args => args[0] === "delete")).toBe(false);
    const activation = f.grant().spec.privateActivation;
    expect(activation.namespaces.find((s: any) => s.namespace.name === "core").epoch)
      .toBe(f.grant("second").spec.privateActivation.namespaces.find((s: any) => s.namespace.name === "core").epoch);
    const qualified = await f.document();
    f.calls.length = 0;
    await applyReviewedGrant(f.execute, qualified);
    expect(f.calls.some(args => args[0] === "patch" && args[1] !== "karscredentialgrants.kars.azure.com")).toBe(false);
  });

  it.each(["sandbox-uid", "namespace-uid", "claim", "deployment-owner", "task-owner", "task-spec", "unobserved",
    "host", "budget-token", "other-private", "pod-owner", "pod-template", "job", "secret-provenance", "missing-secret"])(
    "refuses unsupported %s without any mutation", async fault => {
      const f = await setup();
      if (fault === "sandbox-uid") f.sandbox.metadata.uid = "replaced";
      if (fault === "namespace-uid") f.namespace.metadata.uid = "replaced";
      if (fault === "claim") delete f.namespace.metadata.annotations["kars.azure.com/namespace-claim-version"];
      if (fault === "deployment-owner") f.deployment.metadata.ownerReferences = [{ kind: "KarsSandbox" }];
      if (fault === "task-owner") f.sandbox.metadata.ownerReferences[0].uid = "replaced";
      if (fault === "task-spec") { f.task.metadata.generation++; f.task.status.observedGeneration++; }
      if (fault === "unobserved") f.sandbox.metadata.generation++;
      if (fault === "host") f.deployment.spec.template.spec.hostPID = true;
      if (fault === "budget-token") f.deployment.spec.template.spec.volumes.push({
        name: "budget", projected: { sources: [{ serviceAccountToken: { audience: "kars.azure.com/governed-inference-budget" } }] },
      });
      if (fault === "other-private") f.objects.set(f.key("secret", "router-github-app", "kars-late"), {
        metadata: { name: "router-github-app", uid: "app", resourceVersion: "1" },
      });
      if (fault === "pod-owner") f.pods.get("kars-late")![0].metadata.ownerReferences[0].uid = "replaced";
      if (fault === "pod-template") f.pods.get("kars-late")![0].spec.hostNetwork = true;
      if (fault === "job") f.pods.get("kars-late")![0].metadata.ownerReferences[0].kind = "Job";
      if (fault === "secret-provenance") delete f.secret.metadata.annotations[SOURCE];
      if (fault === "missing-secret") f.objects.delete(f.key("secret", ADMIN, "kars-late"));
      await expect(f.document()).rejects.toThrow();
      expect(f.calls.every(args => args[0] === "get")).toBe(true);
    });

  it.each(["template", "deployment-rv", "namespace-rv", "sandbox-spec"])("rejects changed %s after preview", async fault => {
    const f = await setup();
    const review = await f.document();
    if (fault === "template") f.deployment.spec.template.spec.containers[0].image = "changed";
    if (fault === "deployment-rv") f.deployment.metadata.resourceVersion = "2";
    if (fault === "namespace-rv") f.namespace.metadata.resourceVersion = "2";
    if (fault === "sandbox-spec") { f.sandbox.spec.isolation = "changed"; f.sandbox.metadata.generation++; }
    f.calls.length = 0;
    await expect(applyReviewedGrant(f.execute, review)).rejects.toThrow();
    expect(f.calls.every(args => ["get", "auth"].includes(args[0]!))).toBe(true);
  });

  it.each(["Pausing", "suspend", "Retired", "Rotating", "Restoring", "resume", "Qualified"])(
    "resumes lost acknowledgement at %s with original identity, attempt, captured intent and epoch", async boundary => {
      const f = await setup(false);
      const before = f.preserved();
      let interrupted = false;
      const lost: Execute = async (args, input) => {
        const result = await f.execute(args, input);
        if (args[0] !== "patch" || interrupted) return result;
        const patch = JSON.parse(args[args.indexOf("-p") + 1]!);
        const phase = args[1] === "namespace" && args[2] === "kars-late"
          ? JSON.parse(f.namespace.metadata.annotations[HISTORY]).phase : undefined;
        if (phase === boundary || (args[1] === "karssandbox"
          && ((boundary === "suspend" && patch.spec.suspended === true) || (boundary === "resume" && patch.spec.suspended === false)))) {
          interrupted = true;
          throw new Error("lost acknowledgement");
        }
        return result;
      };
      await expect(applyReviewedGrant(lost, await f.document(lost))).rejects.toThrow("lost acknowledgement");
      expect(interrupted).toBe(true);
      const state = JSON.parse(f.namespace.metadata.annotations[HISTORY]);
      const review = await f.document();
      await applyReviewedGrant(f.execute, review);
      const finished = JSON.parse(f.namespace.metadata.annotations[HISTORY]);
      expect(state.runtime.task.authorization).toBe(AUTHORIZATION);
      expect(finished.runtime.task.authorization).toBe(AUTHORIZATION);
      expect(finished.attempt).toBe(state.attempt);
      expect(finished.epoch).toBe(state.epoch ?? finished.epoch);
      expect(finished.phase).toBe("Qualified");
      expect(finished.captured).toEqual(["old-running", "old-terminating"]);
      expect(f.sandbox.spec.suspended).toBe(false);
      expect(f.preserved()).toEqual(before);
    });

  it("rejects public epoch and version changes when actual old authentication bytes were reused", async () => {
    const f = await setup();
    f.refuseRotation();
    const before = f.preserved();
    await expect(applyReviewedGrant(f.execute, await f.document())).rejects.toThrow("key was not rotated");
    expect(f.preserved()).toEqual(before);
    expect(f.sandbox.spec.suspended).toBe(true);
    expect(f.deployment.spec.replicas).toBe(0);
    expect(JSON.parse(f.namespace.metadata.annotations[HISTORY]).phase).toBe("Rotating");
  });

  it("does not adopt a custom token payload or extra keys", async () => {
    for (const data of [{ "control-token": Buffer.from("custom").toString("base64") },
      { "control-token": Buffer.alloc(64, 193).toString("base64") },
      { "control-token": Buffer.from("A".repeat(64)).toString("base64"), other: "private" }]) {
      const f = await setup();
      f.secret.data = data;
      await expect(applyReviewedGrant(f.execute, await f.document())).rejects.toThrow("Customized or missing");
      expect(f.namespace.metadata.annotations[HISTORY]).toBeUndefined();
    }
  });

  it.each(["a".repeat(64), `SHA256:${"a".repeat(64)}`, `sha256:${"A".repeat(64)}`, "sha256:",
    `sha256:${"a".repeat(63)}`, `sha256:${"a".repeat(65)}`, `${AUTHORIZATION}\n`])(
    "rejects malformed Task authorization %s even if configured identity repeats it", async authorization => {
      const f = await setup();
      f.task.status.envelopeDigest = authorization;
      const env = f.deployment.spec.template.spec.containers[0].env[0];
      env.value = JSON.stringify({ ...JSON.parse(env.value), task_authorization: authorization });
      await expect(f.document()).rejects.toThrow("Task authorization");
      expect(f.calls.every(args => args[0] === "get")).toBe(true);
    });

  it("rejects a different valid prefixed Task digest than the reviewed configured identity", async () => {
    const f = await setup();
    f.task.status.envelopeDigest = `sha256:${"b".repeat(64)}`;
    await expect(f.document()).rejects.toThrow();
    expect(f.calls.every(args => args[0] === "get")).toBe(true);
  });

  it.each(["a".repeat(64), `sha256:${"b".repeat(64)}`])(
    "does not normalize or replace a changed recovery Task authorization %s", async authorization => {
      const f = await setup();
      const stop: Execute = async (args, input) => {
        const value = await f.execute(args, input);
        if (args[0] === "patch" && args[1] === "namespace" && args[2] === "kars-late") throw new Error("interrupted");
        return value;
      };
      await expect(applyReviewedGrant(stop, await f.document(stop))).rejects.toThrow("interrupted");
      const state = JSON.parse(f.namespace.metadata.annotations[HISTORY]);
      expect(state.runtime.task.authorization).toBe(AUTHORIZATION);
      state.runtime.task.authorization = authorization;
      f.namespace.metadata.annotations[HISTORY] = canonical(state);
      f.calls.length = 0;
      await expect(f.document()).rejects.toThrow();
      expect(f.calls.every(args => args[0] === "get")).toBe(true);
      expect(f.namespace.metadata.annotations[`${P}epoch`]).toBeUndefined();
    });

  it("keeps terminating consumers suspended and does not request new authority until actual retirement", async () => {
    const f = await setup();
    f.keepPods();
    const token = f.secret.data["control-token"];
    const delayed: Execute = async (args, input) => {
      const value = await f.execute(args, input);
      if (args[0] === "patch" && args[1] === "deployments.apps") {
        const elapsed = Date.now() + 121_000;
        vi.spyOn(Date, "now").mockReturnValue(elapsed);
      }
      return value;
    };
    await expect(applyReviewedGrant(delayed, await f.document(delayed))).rejects.toThrow("including terminating UIDs");
    expect(f.sandbox.spec.suspended).toBe(true);
    expect(f.secret.data["control-token"]).toBe(token);
    expect(f.namespace.metadata.annotations[`${P}epoch`]).toBeUndefined();
    expect(f.pods.get("kars-late")).toHaveLength(2);
    expect(JSON.parse(f.namespace.metadata.annotations[HISTORY]).captured).toEqual(["old-running", "old-terminating"]);
    vi.mocked(Date.now).mockRestore();
    f.pods.set("kars-late", []);
    await applyReviewedGrant(f.execute, await f.document());
    expect(f.secret.data["control-token"]).not.toBe(token);
  });

  it.each(["karssandbox", "deployments.apps"])("does not overwrite a concurrent %s resourceVersion and safely resumes its original receipt", async kind => {
    const f = await setup();
    const value = kind === "karssandbox" ? f.sandbox : f.deployment;
    let conflicted = false;
    const conflict: Execute = async (args, input) => {
      if (!conflicted && args[0] === "patch" && args[1] === kind) {
        conflicted = true;
        value.metadata.resourceVersion = String(Number(value.metadata.resourceVersion) + 1);
      }
      return f.execute(args, input);
    };
    await expect(applyReviewedGrant(conflict, await f.document(conflict))).rejects.toThrow();
    expect(conflicted).toBe(true);
    const state = JSON.parse(f.namespace.metadata.annotations[HISTORY]);
    expect(state.phase).toBe("Pausing");
    expect(f.secret.data["control-token"]).toBe(Buffer.from("A".repeat(64)).toString("base64"));
    await applyReviewedGrant(f.execute, await f.document());
    expect(JSON.parse(f.namespace.metadata.annotations[HISTORY]).attempt).toBe(state.attempt);
  });

  it("rejects a key minted before the recorded retirement boundary instead of blessing its bytes", async () => {
    const f = await setup();
    const premature: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (args[0] === "patch" && args[1] === "deployments.apps") {
        f.secret.data["control-token"] = Buffer.from("C".repeat(64)).toString("base64");
        f.secret.metadata.resourceVersion = "2";
      }
      return result;
    };
    await expect(applyReviewedGrant(premature, await f.document(premature))).rejects.toThrow("key changed before retirement");
    expect(f.sandbox.spec.suspended).toBe(true);
    expect(f.namespace.metadata.annotations[`${P}epoch`]).toBeUndefined();
    expect(JSON.parse(f.namespace.metadata.annotations[HISTORY]).phase).toBe("Pausing");
  });

  it.each(["spec", "task", "uid", "receipt", "provenance", "new-pod"])("preserves suspension on changed recovery %s", async fault => {
    const f = await setup();
    const stop: Execute = async (args, input) => {
      const value = await f.execute(args, input);
      if (args[0] === "patch" && args[1] === "namespace" && args[2] === "kars-late"
        && JSON.parse(f.namespace.metadata.annotations[HISTORY]).phase === "Rotating") throw new Error("interrupted");
      return value;
    };
    await expect(applyReviewedGrant(stop, await f.document(stop))).rejects.toThrow("interrupted");
    if (fault === "spec") f.sandbox.spec.isolation = "changed";
    if (fault === "task") f.task.spec.objective = "changed";
    if (fault === "uid") f.sandbox.metadata.uid = "changed";
    if (fault === "receipt") delete f.namespace.metadata.annotations[HISTORY];
    if (fault === "provenance") delete f.secret.metadata.annotations[SOURCE];
    if (fault === "new-pod") f.pods.set("kars-late", [{ kind: "Pod", metadata: { name: "new", uid: "new", resourceVersion: "1" }, spec: { containers: [] } }]);
    f.calls.length = 0;
    await expect(f.document()).rejects.toThrow();
    expect(f.calls.every(args => args[0] === "get")).toBe(true);
    expect(f.sandbox.spec.suspended).toBe(true);
    expect(f.deployment.spec.replicas).toBe(0);
  });
});
