// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { EventEmitter } from "node:events";
import { readFileSync } from "node:fs";
import { spawn } from "node:child_process";
import { describe, expect, it, vi } from "vitest";

const { ownedRouterResolver, waitForOwnedRouter, startForward, FixtureIdentityError, ForwardUnavailable } =
  await import(new URL("../../../tests/e2e/budget-router-readiness.mjs", import.meta.url).href);

const name = "budget-money-left";
const workspace = "kars-system";
const runtime = `kars-${name}`;
const image = `kars-inference-router:e2e@sha256:${"a".repeat(64)}`;
const ready = { status: 200, value: "governed inference authority and model contracts available" };
const unavailable = { status: 503, value: "not ready — governed inference authority, provider or contracts unavailable" };
const secret = "DO-NOT-EXPORT-PRIVATE-BINDING-BODY-OR-ARGV";
const owner = (kind: string, name: string, uid: string, apiVersion = "apps/v1") =>
  ({ kind, apiVersion, name, uid, controller: true });
const annotation = (suffix: string) => `kars.azure.com/${suffix}`;

function fixture() {
  const task = {
    metadata: { name, namespace: workspace, uid: "task-uid", generation: 1 },
    spec: { execution: { launch: true } },
    status: { phase: "Ready", observedGeneration: 1, inferenceBudget: {
      taskUid: "task-uid", account: { name: "account", namespace: workspace, uid: "account-uid" },
      authorizationDigest: secret,
    } },
  };
  const sandbox = {
    metadata: { name, namespace: workspace, uid: "sandbox-uid",
      ownerReferences: [owner("KarsTask", name, task.metadata.uid, "kars.azure.com/v1alpha1")],
      annotations: { [annotation("namespace-uid")]: "namespace-uid" } },
    spec: { inferenceBudgetRef: structuredClone(task.status.inferenceBudget) },
  };
  const namespace = { metadata: { name: runtime, uid: "namespace-uid", annotations: {
    [annotation("namespace-claim-version")]: "v1", [annotation("sandbox-namespace")]: workspace,
    [annotation("sandbox-name")]: name, [annotation("sandbox-uid")]: sandbox.metadata.uid,
  }, labels: { [annotation("inference-budget")]: "v1" } } };
  const binding = { task: task.status.inferenceBudget,
    sandbox: { name, namespace: workspace, uid: sandbox.metadata.uid },
    runtimeNamespace: runtime, runtimeNamespaceUid: namespace.metadata.uid, privacyEpoch: null };
  const labels = { [annotation("sandbox")]: name };
  const template = { metadata: { labels, annotations: { revision: "1" } },
    spec: { serviceAccountName: "sandbox", containers: [{ name: "inference-router", image,
      env: [{ name: "KARS_INFERENCE_BUDGET_BINDING", value: JSON.stringify(binding) }],
      args: [secret], volumeMounts: [] as object[] }] } };
  const deployment = {
    metadata: { name, namespace: runtime, uid: "deployment-uid", generation: 1,
      labels: { ...labels, [annotation("parent-namespace")]: workspace } },
    spec: { replicas: 1, selector: { matchLabels: labels }, template },
    status: { observedGeneration: 1 },
  };
  const policy = { spec: { modelPreference: { primary: { provider: "budget-fixture", deployment: "fixture" } } } };
  // Deliberately mutable API snapshots let each test model real UID/rollout races.
  const objects = new Map<string, any>([
    ["karstask", task], ["karssandbox", sandbox], ["namespace", namespace], ["deployment", deployment],
    ["inferencepolicy", policy],
  ]);
  const state = { pods: [] as any[], time: 0 };
  const read = vi.fn(async (kind: string, resourceName: string) =>
    objects.get(kind === "replicaset" ? resourceName : kind) ?? null);
  const pods = vi.fn(async () => state.pods);
  function addPod(revision: number) {
    const replicaName = `router-rs-${revision}`;
    const replica = { metadata: { name: replicaName, namespace: runtime, uid: `rs-${revision}`,
      ownerReferences: [owner("Deployment", name, deployment.metadata.uid)] },
      spec: { template: structuredClone(deployment.spec.template) } };
    const pod = { metadata: { name: `router-pod-${revision}`, namespace: runtime, uid: `pod-${revision}`,
      labels, ownerReferences: [owner("ReplicaSet", replicaName, replica.metadata.uid)] },
      spec: structuredClone(replica.spec.template.spec),
      status: { phase: "Running", containerStatuses: [{ name: "inference-router", state: { running: {} } }] } };
    pod.spec.containers[0].volumeMounts.push({ name: "kube-api-access-12345", readOnly: true,
      mountPath: "/var/run/secrets/kubernetes.io/serviceaccount" });
    objects.set(replicaName, replica);
    state.pods.push(pod);
    return pod;
  }
  const pod = addPod(1);
  const roll = () => {
    const revision = ++deployment.metadata.generation;
    deployment.status.observedGeneration = revision;
    deployment.spec.template.metadata.annotations.revision = String(revision);
    return addPod(revision);
  };
  const resolve = ownedRouterResolver({ task: structuredClone(task), image, read, pods, deadline: 2000 });
  const forwards: { alive: ReturnType<typeof vi.fn>; stop: ReturnType<typeof vi.fn>; url: string }[] = [];
  const openForward = vi.fn(async () => {
    const forward = { alive: vi.fn(() => true), stop: vi.fn(async () => {}),
      url: `http://127.0.0.1:${10000 + forwards.length}` };
    forwards.push(forward);
    return forward;
  });
  const report = vi.fn();
  const options = { resolve, openForward, report, deadline: 2000, now: () => state.time,
    sleep: async (ms: number) => { state.time += ms; },
    probe: vi.fn(async (_url: string, path: string) => path === "/healthz" ? { status: 200, value: "ok" } : ready) };
  return { task, sandbox, namespace, deployment, pod, objects, state, read, pods, roll,
    resolve, openForward, forwards, report, options };
}

describe("UID-fenced native budget router readiness", () => {
  it("waits for late creation without swallowing API errors or adopting identities", async () => {
    const f = fixture();
    const delayed = f.objects.get("deployment");
    f.objects.delete("deployment");
    f.options.sleep = async ms => {
      f.state.time += ms;
      f.objects.set("deployment", delayed);
    };
    const result = await waitForOwnedRouter(f.options);
    expect(result.pod.metadata.uid).toBe("pod-1");
    expect(f.openForward).toHaveBeenCalledTimes(1);
    expect(f.state.time).toBe(500);
    expect(f.options.probe.mock.calls.map(call => call[1])).toEqual(["/healthz", "/readyz"]);
    expect(f.forwards[0].stop).not.toHaveBeenCalled();
  });

  it("waits for late Pod creation and refuses an ambiguous initial Pod choice", async () => {
    const f = fixture();
    f.state.pods = [];
    expect(await f.resolve()).toBeNull();
    f.state.pods = [f.pod, { ...f.pod, metadata: { ...f.pod.metadata, name: "second", uid: "pod-2" } }];
    expect(await f.resolve()).toBeNull();
    expect((await f.resolve("pod-1"))?.pod.metadata.uid).toBe("pod-1");
    f.state.pods = [f.pod];
    expect((await waitForOwnedRouter(f.options)).pod.metadata.uid).toBe("pod-1");
  });

  it("reselects a legitimate current-template rollout and stops only its old tunnel", async () => {
    const f = fixture();
    f.options.probe = vi.fn(async (_url, path) => {
      if (path === "/healthz") return { status: 200, value: "ok" };
      if (f.forwards.length === 1) { f.roll(); return unavailable; }
      return ready;
    });
    const result = await waitForOwnedRouter(f.options);
    expect(result.pod.metadata.uid).toBe("pod-2");
    expect(f.openForward).toHaveBeenCalledTimes(2);
    expect(f.forwards[0].stop).toHaveBeenCalledTimes(1);
    expect(f.forwards[1].stop).not.toHaveBeenCalled();
    expect(JSON.stringify(f.report.mock.calls)).not.toContain(secret);
  });

  it("ignores terminating Pods and stale ReplicaSet templates, not current ownership", async () => {
    const f = fixture();
    f.roll();
    expect((await f.resolve())?.pod.metadata.uid).toBe("pod-2");
    f.state.pods[1].metadata.deletionTimestamp = "2026-09-10T00:00:00Z";
    expect(await f.resolve()).toBeNull();
    expect(f.openForward).not.toHaveBeenCalled();
  });

  it.each(["karstask", "karssandbox", "namespace", "deployment"])(
    "rejects a same-name recreated %s, even after a transport failure", async kind => {
      const f = fixture();
      f.options.probe = vi.fn(async () => {
        f.objects.get(kind).metadata.uid = "foreign-uid";
        throw new Error(secret);
      });
      await expect(waitForOwnedRouter(f.options)).rejects.toBeInstanceOf(FixtureIdentityError);
      expect(f.openForward).toHaveBeenCalledTimes(1);
      expect(f.forwards[0].stop).toHaveBeenCalledTimes(1);
      expect(JSON.stringify(f.report.mock.calls)).not.toContain(secret);
    },
  );

  it.each(["sandbox-owner", "namespace-claim", "deployment-scope", "replica-owner", "pod-owner",
    "image", "binding", "pod-environment", "mount", "account"])("fails closed for %s", async fault => {
    const f = fixture();
    const replica = f.objects.get("router-rs-1");
    if (fault === "sandbox-owner") f.sandbox.metadata.ownerReferences[0].uid = "foreign";
    if (fault === "namespace-claim") f.namespace.metadata.annotations[annotation("sandbox-uid")] = "foreign";
    if (fault === "deployment-scope") f.deployment.metadata.labels[annotation("parent-namespace")] = "foreign";
    if (fault === "replica-owner") replica.metadata.ownerReferences[0].uid = "foreign";
    if (fault === "pod-owner") f.pod.metadata.ownerReferences[0].uid = "foreign";
    if (fault === "image") f.deployment.spec.template.spec.containers[0].image = "other:latest";
    if (fault === "binding") f.deployment.spec.template.spec.containers[0].env[0].value = secret;
    if (fault === "pod-environment") f.pod.spec.containers[0].env.push({ name: "UNTRUSTED", value: secret });
    if (fault === "mount") f.pod.spec.containers[0].volumeMounts.push({ name: "foreign", mountPath: "/private" });
    if (fault === "account") {
      await f.resolve();
      f.task.status.inferenceBudget.account.uid = "new-zero-account";
    }
    let error: unknown;
    try { await waitForOwnedRouter(f.options); } catch (caught) { error = caught; }
    expect(error).toBeInstanceOf(FixtureIdentityError);
    expect(String(error)).not.toContain(secret);
    expect(f.openForward).not.toHaveBeenCalled();
  });

  it("cannot return Ready when authority changes during successful HTTP responses", async () => {
    const f = fixture();
    f.options.probe = vi.fn(async () => {
      f.objects.get("karstask").metadata.uid = "replacement-task";
      return ready;
    });
    await expect(waitForOwnedRouter(f.options)).rejects.toBeInstanceOf(FixtureIdentityError);
    expect(f.forwards[0].stop).toHaveBeenCalledTimes(1);
  });

  it("checks ownership after tunnel startup and before issuing any HTTP probe", async () => {
    const f = fixture();
    const open = f.options.openForward;
    await expect(waitForOwnedRouter({ ...f.options, openForward: async () => {
      const forward = await open();
      f.objects.get("namespace").metadata.uid = "replacement-namespace";
      return forward;
    } })).rejects.toBeInstanceOf(FixtureIdentityError);
    expect(f.options.probe).not.toHaveBeenCalled();
    expect(f.forwards[0].stop).toHaveBeenCalledTimes(1);
  });

  it("reconnects an expired owned forward to the same verified Pod", async () => {
    const f = fixture();
    f.options.probe = vi.fn(async (_url, path) => {
      if (f.forwards.length === 1) f.forwards[0].alive.mockReturnValue(false);
      return path === "/healthz" ? { status: 200, value: "ok" } : ready;
    });
    expect((await waitForOwnedRouter(f.options)).pod.metadata.uid).toBe("pod-1");
    expect(f.openForward).toHaveBeenCalledTimes(2);
    expect(f.forwards[0].stop).toHaveBeenCalledTimes(1);
  });

  it.each(["503", "transport", "unqualified-200", "healthz-503"])(
    "%s remains blocked until the original deadline, without reconnecting a live tunnel", async failure => {
      const f = fixture();
      f.options.probe = vi.fn(async (_url, path) => {
        if (failure === "transport") throw new Error(secret);
        if (failure === "unqualified-200") return { status: 200, value: "ok" };
        if (failure === "healthz-503") return path === "/healthz" ? unavailable : ready;
        return path === "/healthz" ? { status: 200, value: "ok" } : unavailable;
      });
      await expect(waitForOwnedRouter(f.options)).rejects.toThrow("deadline");
      expect(f.state.time).toBe(2000);
      expect(f.openForward).toHaveBeenCalledTimes(1);
      expect(f.forwards[0].stop).toHaveBeenCalledTimes(1);
      expect(JSON.stringify(f.report.mock.calls)).not.toContain(secret);
    },
  );

  it.each([401, 403])("fails immediately on real HTTP %s authorization rejection", async status => {
    const f = fixture();
    f.options.probe = vi.fn(async () => ({ status, value: secret }));
    await expect(waitForOwnedRouter(f.options)).rejects.toThrow("authorization rejected");
    expect(f.state.time).toBe(0);
    expect(f.openForward).toHaveBeenCalledTimes(1);
    expect(f.forwards[0].stop).toHaveBeenCalledTimes(1);
  });

  it("limits reconnections despite repeated legitimate rolls", async () => {
    const f = fixture();
    f.options.probe = vi.fn(async (_url, path) => {
      if (path === "/readyz") f.roll();
      return unavailable;
    });
    await expect(waitForOwnedRouter({ ...f.options, deadline: 10_000 })).rejects.toThrow("reconnect bound");
    expect(f.openForward).toHaveBeenCalledTimes(4);
    expect(f.forwards.every(forward => forward.stop.mock.calls.length === 1)).toBe(true);
  });

  it("bounds startup failures and retries only the exact owned target", async () => {
    const f = fixture();
    f.openForward.mockRejectedValue(new ForwardUnavailable("Port-forward exited"));
    await expect(waitForOwnedRouter({ ...f.options, deadline: 10_000 })).rejects.toThrow("reconnect bound");
    expect(f.openForward).toHaveBeenCalledTimes(4);
    expect(f.options.probe).not.toHaveBeenCalled();
    expect(f.openForward.mock.calls.every(call => (call as any[])[0].pod.metadata.uid === "pod-1")).toBe(true);
  });

  it("cannot reset the deadline by receiving a late successful response", async () => {
    const f = fixture();
    f.options.probe = vi.fn(async () => {
      f.state.time = 2000;
      return ready;
    });
    await expect(waitForOwnedRouter(f.options)).rejects.toThrow("deadline");
    expect(f.openForward).toHaveBeenCalledTimes(1);
    expect(f.forwards[0].stop).toHaveBeenCalledTimes(1);
  });

  it("does not open a forward when no owned Pod appears before the deadline", async () => {
    const f = fixture();
    f.state.pods = [];
    await expect(waitForOwnedRouter(f.options)).rejects.toThrow("deadline");
    expect(f.state.time).toBe(2000);
    expect(f.openForward).not.toHaveBeenCalled();
  });

  it("propagates non-NotFound API failures and closes the owned tunnel", async () => {
    const f = fixture();
    f.options.probe = vi.fn(async () => {
      f.read.mockRejectedValue(new Error("Sanitized API failure"));
      return unavailable;
    });
    await expect(waitForOwnedRouter(f.options)).rejects.toThrow("Sanitized API failure");
    expect(f.forwards[0].stop).toHaveBeenCalledTimes(1);
    const harness = readFileSync(new URL("../../../tests/e2e/inference-budget-enforcement.mjs", import.meta.url), "utf8");
    expect(harness).toContain('"--ignore-not-found"');
    expect(harness.slice(harness.indexOf("async function until"), harness.indexOf("async function portForward")))
      .not.toContain("catch");
    expect(harness).toContain("task: createdTasks.get(name)");
    expect(harness).toContain("preconditions: { uid: secret.uid }");
  });
});

describe("owned port-forward process lifecycle", () => {
  it("serves a real local probe and reaps only the child that owns that connection", async () => {
    const children: ReturnType<typeof spawn>[] = [];
    const handles: { stop: () => Promise<void> }[] = [];
    try {
      const forward = await startForward({ context: "kind-kars-e2e", cwd: ".", namespace: runtime,
        target: "pod/owned", port: 8443, deadline: Date.now() + 3000,
        register: (handle: { stop: () => Promise<void> }) => handles.push(handle),
        spawnProcess: () => {
          const child = spawn(process.execPath, ["-e", `
            const server = require("node:http").createServer((_, response) => response.end("ok"));
            server.listen(0, "127.0.0.1", () =>
              console.log("Forwarding from 127.0.0.1:" + server.address().port + " -> 8443"));
          `], { stdio: ["ignore", "pipe", "pipe"] });
          children.push(child);
          return child;
        } });
      const response = await fetch(`${forward.url}/healthz`, { signal: AbortSignal.timeout(1000) });
      expect(response.status).toBe(200);
      expect(await response.text()).toBe("ok");
      await forward.stop();
      expect(forward.alive()).toBe(false);
      expect(children[0].signalCode).toBe("SIGTERM");
    } finally {
      await Promise.all(handles.map(handle => handle.stop()));
    }
  });

  function processFixture(output = true) {
    let time = 0;
    const child = Object.assign(new EventEmitter(), {
      pid: 12345, exitCode: null as number | null, signalCode: null as string | null,
      stdout: new EventEmitter(), stderr: new EventEmitter(),
      kill: vi.fn((signal: string) => { child.signalCode = signal; return true; }),
    });
    const register = vi.fn();
    const spawnProcess = vi.fn(() => child);
    const options = { context: "kind-kars-e2e", cwd: ".", namespace: runtime, target: "pod/owned", port: 8443,
      deadline: 1000, register, spawnProcess, now: () => time,
      sleep: async (ms: number) => {
        time += ms;
        if (output) child.stdout.emit("data", Buffer.from("Forwarding from 127.0.0.1:12345 -> 8443\n"));
      } };
    return { child, options, spawnProcess, register };
  }

  it("owns only its created process and cleanup is idempotent", async () => {
    const f = processFixture();
    const unrelated = vi.fn();
    const forward = await startForward(f.options);
    expect(forward.url).toBe("http://127.0.0.1:12345");
    expect(forward.alive()).toBe(true);
    expect(f.register).toHaveBeenCalledWith(forward);
    await Promise.all([forward.stop(), forward.stop()]);
    expect(f.child.kill).toHaveBeenCalledExactlyOnceWith("SIGTERM");
    expect(unrelated).not.toHaveBeenCalled();
    expect(forward.alive()).toBe(false);
  });

  it("cleans up a startup deadline without exposing stderr or command output", async () => {
    const f = processFixture(false);
    const pending = startForward(f.options);
    f.child.stderr.emit("data", Buffer.from(secret));
    await expect(pending).rejects.toThrow("startup deadline");
    expect(f.child.kill).toHaveBeenCalledExactlyOnceWith("SIGTERM");
  });

  it("detects an exited forward immediately rather than swallowing it until timeout", async () => {
    const f = processFixture();
    f.child.exitCode = 1;
    await expect(startForward(f.options)).rejects.toBeInstanceOf(ForwardUnavailable);
    expect(f.child.kill).not.toHaveBeenCalled();
  });

  it("escalates only its unresponsive child and awaits its exit", async () => {
    const f = processFixture();
    f.child.kill.mockImplementation(signal => {
      if (signal === "SIGKILL") f.child.signalCode = signal;
      return true;
    });
    const forward = await startForward(f.options);
    await forward.stop();
    expect(f.child.kill.mock.calls).toEqual([["SIGTERM"], ["SIGKILL"]]);
    expect(forward.alive()).toBe(false);
  });
});
