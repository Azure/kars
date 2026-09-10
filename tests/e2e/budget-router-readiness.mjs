// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { spawn } from "node:child_process";
import { isDeepStrictEqual as equal } from "node:util";
import { readinessFact, routerTemplateFacts, verifyFixturePolicy } from "./budget-fixture-route.mjs";

const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const annotation = (object, suffix) => object.metadata?.annotations?.[`kars.azure.com/${suffix}`];
const label = (object, suffix) => object.metadata?.labels?.[`kars.azure.com/${suffix}`];

export class FixtureIdentityError extends Error {}
export class ForwardUnavailable extends Error {}

function requireIdentity(condition, message) {
  if (!condition) throw new FixtureIdentityError(`Budget fixture identity: ${message}`);
}

function owner(object, kind, apiVersion) {
  const owners = object.metadata?.ownerReferences?.filter(ref => ref.controller === true) ?? [];
  requireIdentity(owners.length === 1 && owners[0].kind === kind && owners[0].apiVersion === apiVersion
    && owners[0].name && owners[0].uid, "controller owner is missing or foreign");
  return owners[0];
}

function ownedBy(object, parent, kind, apiVersion) {
  const ref = owner(object, kind, apiVersion);
  requireIdentity(ref.name === parent.metadata.name && ref.uid === parent.metadata.uid,
    "controller owner UID differs");
}

function binding(container) {
  const entries = container?.env?.filter(entry => entry.name === "KARS_INFERENCE_BUDGET_BINDING") ?? [];
  requireIdentity(entries.length === 1 && typeof entries[0].value === "string" && !entries[0].valueFrom,
    "private budget binding is missing or ambiguous");
  try { return JSON.parse(entries[0].value); }
  catch { throw new FixtureIdentityError("Budget fixture identity: private budget binding is malformed"); }
}

function routerContainer(spec) {
  const containers = spec?.containers?.filter(container => container.name === "inference-router") ?? [];
  requireIdentity(containers.length === 1, "exactly one real router is required");
  return containers[0];
}

function sameTemplate(replica, deployment) {
  const labels = template => Object.fromEntries(Object.entries(template.metadata?.labels ?? {})
    .filter(([key]) => key !== "pod-template-hash"));
  return equal(replica.spec?.template?.spec, deployment.spec.template.spec)
    && equal(replica.spec?.template?.metadata?.annotations ?? {}, deployment.spec.template.metadata?.annotations ?? {})
    && equal(labels(replica.spec?.template ?? {}), labels(deployment.spec.template));
}

function mountsMatch(actual, template) {
  const expected = template.volumeMounts ?? [];
  const mounts = actual.volumeMounts ?? [];
  // Match the broker's router_mounts_match rule, including API-injected SA mounts.
  return expected.every(mount => mounts.some(value => equal(value, mount)))
    && mounts.filter(mount => !expected.some(value => equal(value, mount))).every(mount =>
      mount.readOnly === true && mount.subPath == null && mount.subPathExpr == null && mount.mountPropagation == null
      && ((mount.name?.startsWith("kube-api-access-")
        && mount.mountPath === "/var/run/secrets/kubernetes.io/serviceaccount")
        || (mount.name === "azure-identity-token" && mount.mountPath === "/var/run/secrets/azure/tokens")));
}

// The Task UID comes from this fixture's create response, never a name-only GET.
// Pin each later identity on first observation; missing resources may appear late,
// but a same-name replacement must never become a new readiness authority.
export function ownedRouterResolver({ task, image, read, pods, deadline }) {
  const name = task?.metadata?.name;
  const workspace = task?.metadata?.namespace;
  const runtime = `kars-${name}`;
  requireIdentity(name && workspace && task.metadata.uid && task.metadata.generation, "created Task is required");
  requireIdentity(/^kars-inference-router:e2e@sha256:[a-f0-9]{64}$/.test(image), "verified fixture image is required");
  const uids = new Map([["karstask", task.metadata.uid]]);
  let taskBinding;

  function pin(kind, object, expectedName, namespace) {
    requireIdentity(object.metadata?.name === expectedName && object.metadata?.namespace === namespace
      && object.metadata?.uid && !object.metadata.deletionTimestamp, "resource identity or lifetime differs");
    requireIdentity(!uids.has(kind) || uids.get(kind) === object.metadata.uid, "pinned resource UID changed");
    uids.set(kind, object.metadata.uid);
  }

  return async preferredUid => {
    const current = await read("karstask", name, workspace, deadline);
    if (!current) return null;
    pin("karstask", current, name, workspace);
    requireIdentity(current.metadata.generation === task.metadata.generation
      && current.spec?.execution?.launch === true, "Task authority changed");
    const bound = current.status?.inferenceBudget;
    if (taskBinding) requireIdentity(equal(bound, taskBinding), "Task account binding changed");
    if (current.status?.phase !== "Ready" || current.status?.observedGeneration !== current.metadata.generation
        || !bound) return null;
    requireIdentity(bound.taskUid === current.metadata.uid && bound.account?.uid
      && bound.authorizationDigest, "Task account authority is incomplete");
    taskBinding ??= structuredClone(bound);
    const policy = await read("inferencepolicy", `${name}-inference`, workspace, deadline);
    if (!policy) return null;
    verifyFixturePolicy(policy);

    const sandbox = await read("karssandbox", name, workspace, deadline);
    if (!sandbox) return null;
    pin("karssandbox", sandbox, name, workspace);
    ownedBy(sandbox, current, "KarsTask", "kars.azure.com/v1alpha1");
    requireIdentity(equal(sandbox.spec?.inferenceBudgetRef, bound), "Sandbox budget authority differs");
    const namespace = await read("namespace", runtime, undefined, deadline);
    if (!namespace) return null;
    pin("namespace", namespace, runtime, undefined);
    requireIdentity(!(namespace.metadata.ownerReferences?.length), "runtime Namespace has a foreign owner");
    let claimed = true;
    for (const [suffix, expected] of [
      ["namespace-claim-version", "v1"], ["sandbox-namespace", workspace],
      ["sandbox-name", name], ["sandbox-uid", sandbox.metadata.uid],
    ]) {
      const value = annotation(namespace, suffix);
      requireIdentity(value === undefined || value === expected, "runtime Namespace claim differs");
      claimed &&= value !== undefined;
    }
    if (!claimed) return null;
    const namespaceUid = annotation(sandbox, "namespace-uid");
    requireIdentity(namespaceUid === undefined || namespaceUid === namespace.metadata.uid,
      "Sandbox runtime Namespace UID differs");
    requireIdentity(annotation(namespace, "namespace-prestage") === undefined, "runtime Namespace claim is still prestaged");
    if (!namespaceUid) return null;
    const fence = label(namespace, "inference-budget");
    requireIdentity(fence === undefined || fence === "v1", "runtime budget fence differs");
    if (!fence) return null;

    const deployment = await read("deployment", name, runtime, deadline);
    if (!deployment) return null;
    pin("deployment", deployment, name, runtime);
    requireIdentity(label(deployment, "sandbox") === name && label(deployment, "parent-namespace") === workspace
      && !(deployment.metadata.ownerReferences?.length)
      && equal(deployment.spec?.selector?.matchLabels, { "kars.azure.com/sandbox": name })
      && !(deployment.spec.selector.matchExpressions?.length), "Deployment workload scope differs");
    const desired = routerContainer(deployment.spec?.template?.spec);
    requireIdentity(desired.image === image, "Deployment is not the exact CRI-verified image");
    const desiredBinding = binding(desired);
    requireIdentity(equal(desiredBinding.task, bound)
      && equal(desiredBinding.sandbox, { name, namespace: workspace, uid: sandbox.metadata.uid })
      && desiredBinding.runtimeNamespace === runtime
      && desiredBinding.runtimeNamespaceUid === namespace.metadata.uid, "Deployment budget binding differs");
    if (!deployment.metadata.generation || deployment.status?.observedGeneration !== deployment.metadata.generation)
      return null;

    const candidates = [];
    for (const pod of await pods(runtime, name, deadline)) {
      requireIdentity(pod.metadata?.namespace === runtime && pod.metadata?.uid
        && label(pod, "sandbox") === name, "Pod workload scope differs");
      const replicaOwner = owner(pod, "ReplicaSet", "apps/v1");
      const replica = await read("replicaset", replicaOwner.name, runtime, deadline);
      if (!replica) continue;
      requireIdentity(replica.metadata?.namespace === runtime
        && replica.metadata?.name === replicaOwner.name, "ReplicaSet scope differs");
      ownedBy(pod, replica, "ReplicaSet", "apps/v1");
      ownedBy(replica, deployment, "Deployment", "apps/v1");
      if (pod.metadata.deletionTimestamp || replica.metadata.deletionTimestamp || !sameTemplate(replica, deployment))
        continue;
      const actual = routerContainer(pod.spec);
      const facts = routerTemplateFacts(pod, replica, deployment);
      requireIdentity(Object.entries(facts).every(([key, value]) =>
        key === "stage" || key === "volumeMountsMatch" || value === true)
        && actual.image === image && equal(binding(actual), desiredBinding) && mountsMatch(actual, desired)
        && pod.spec.serviceAccountName === "sandbox", "current Pod template or private binding differs");
      if (pod.status?.phase === "Running" && pod.status.containerStatuses?.some(
        container => container.name === "inference-router" && container.state?.running)) {
        candidates.push({ pod, runtime, facts, deploymentGeneration: deployment.metadata.generation });
      }
    }
    return candidates.find(candidate => candidate.pod.metadata.uid === preferredUid)
      ?? (candidates.length === 1 ? candidates[0] : null);
  };
}

export async function startForward({ context, cwd, namespace, target, port, deadline,
  register = () => {}, spawnProcess = spawn, now = Date.now, sleep = pause }) {
  const child = spawnProcess("kubectl", ["--context", context, "port-forward", "--address", "127.0.0.1",
    "-n", namespace, target, `:${port}`], { cwd, stdio: ["ignore", "pipe", "pipe"] });
  let output = "", failed = false, stopping;
  child.stdout.on("data", data => { output = (output + data).slice(-2048); });
  child.stderr.on("data", () => {});
  child.on("error", () => { failed = true; });
  const exited = () => child.exitCode !== null || child.signalCode !== null;
  const handle = {
    alive: () => !failed && !exited(),
    stop: () => stopping ??= (async () => {
      if (exited() || !child.pid) return;
      child.kill("SIGTERM");
      const end = now() + 2000;
      while (!exited() && now() < end) await sleep(Math.min(50, end - now()));
      if (!exited()) {
        child.kill("SIGKILL");
        const killed = now() + 2000;
        while (!exited() && now() < killed) await sleep(Math.min(50, killed - now()));
        if (!exited()) throw new Error("Budget fixture owned port-forward cleanup failed");
      }
    })(),
  };
  register(handle);
  try {
    const end = Math.min(deadline, now() + 30_000);
    while (now() < end) {
      if (!handle.alive()) throw new ForwardUnavailable("Budget fixture port-forward exited");
      const match = output.match(/Forwarding from 127\.0\.0\.1:(\d+) ->/);
      if (match) return Object.assign(handle, { url: `http://127.0.0.1:${match[1]}` });
      await sleep(Math.min(50, end - now()));
    }
    throw new ForwardUnavailable("Budget fixture port-forward startup deadline");
  } catch (error) {
    await handle.stop();
    throw error;
  }
}

export async function waitForOwnedRouter({ resolve, openForward, probe, deadline,
  maxReconnects = 3, report = () => {}, now = Date.now, sleep = pause }) {
  requireIdentity(Number.isInteger(maxReconnects) && maxReconnects >= 0 && maxReconnects <= 3
    && Number.isFinite(deadline), "readiness bounds are invalid");
  let active, forward, attempts = 0;
  const lastReadiness = new Map();
  const stop = async () => {
    const previous = forward;
    forward = undefined;
    if (previous) await previous.stop();
  };
  try {
    while (now() < deadline) {
      const selected = await resolve(active?.pod.metadata.uid);
      if (now() >= deadline) break;
      if (!selected) {
        await stop();
      } else {
        const changed = active?.pod.metadata.uid !== selected.pod.metadata.uid;
        if (changed || !forward?.alive()) {
          await stop();
          if (now() >= deadline) break;
          if (attempts >= maxReconnects + 1) throw new Error("Budget fixture port-forward reconnect bound");
          active = selected;
          attempts++;
          report({ stage: "router-owned-target", podUid: active.pod.metadata.uid, attempt: attempts });
          try { forward = await openForward(active, deadline); }
          catch (error) { if (!(error instanceof ForwardUnavailable)) throw error; }
        }
        // kubectl resolves a Pod name while opening the tunnel. Fence that
        // asynchronous window before even issuing the first readiness probe.
        const connected = forward?.alive() ? await resolve(active.pod.metadata.uid) : null;
        if (connected?.pod.metadata.uid === active?.pod.metadata.uid
            && connected.deploymentGeneration === selected.deploymentGeneration
            && forward?.alive()) {
          let ready = true;
          for (const path of ["/healthz", "/readyz"]) {
            if (now() >= deadline) { ready = false; break; }
            let response, error;
            try { response = await probe(forward.url, path, Math.min(30_000, deadline - now())); }
            catch (caught) {
              if (caught instanceof FixtureIdentityError) throw caught;
              error = caught;
            }
            const fact = readinessFact(path, response, error);
            const key = JSON.stringify({ ...fact, podUid: active.pod.metadata.uid });
            if (lastReadiness.get(path) !== key) {
              report({ ...fact, podUid: active.pod.metadata.uid });
              lastReadiness.set(path, key);
            }
            if (response?.status === 401 || response?.status === 403)
              throw new Error("Budget fixture readiness authorization rejected");
            ready &&= fact.ready;
          }
          // A successful HTTP response does not authorize a replaced workload.
          const current = await resolve(active.pod.metadata.uid);
          if (ready && now() < deadline && forward.alive()
              && current?.pod.metadata.uid === active.pod.metadata.uid
              && current.deploymentGeneration === selected.deploymentGeneration) {
            return { ...current, url: forward.url };
          }
        }
      }
      if (now() < deadline) await sleep(Math.min(500, deadline - now()));
    }
    throw new Error("Budget fixture deadline: owned router private readiness");
  } catch (error) {
    await stop();
    throw error;
  }
}
