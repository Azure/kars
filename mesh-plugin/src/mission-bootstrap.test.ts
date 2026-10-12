// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { dispatcherCustody, dispatcherRootName, type DispatcherLocation } from "./mission-bootstrap.js";
import type { KubernetesJson } from "./kubernetes-json.js";
import { dispatcherEntraName, dispatcherEntraPin, type DispatcherEntraBinding } from "./mission-entra.js";

const p = "kars.azure.com/";
const root = "a".repeat(64);
const location: DispatcherLocation = { namespace: "kars-system", deployment: "kars-mission-dispatcher", release: "kars", podName: "dispatcher-abc", podUid: "pod-uid" };
const entra = { tenantId: "a1111111-1111-4111-8111-111111111111", blueprintId: "b2222222-2222-4222-8222-222222222222" };
function fixture(withEntra = false) {
  const nsPath = `/api/v1/namespaces/${location.namespace}`;
  const deploymentPath = `/apis/apps/v1/namespaces/${location.namespace}/deployments/${location.deployment}`;
  const rsPath = `/apis/apps/v1/namespaces/${location.namespace}/replicasets/dispatcher-rs`;
  const podPath = `${nsPath}/pods/${location.podName}`;
  const secretPath = `${nsPath}/secrets/${dispatcherRootName}`;
  const meta = (name: string, uid: string) => ({ name, uid, namespace: location.namespace, resourceVersion: "1", generation: 1 });
  const owner = (kind: string, name: string, uid: string) => ({ apiVersion: "apps/v1", kind, name, uid, controller: true, blockOwnerDeletion: true });
  const ns = { metadata: { name: location.namespace, uid: "namespace-uid", resourceVersion: "1" }, status: { phase: "Active" } };
  const template = { metadata: { labels: { app: "dispatcher" } as Record<string, string> }, spec: { containers: [{ name: "mission-dispatcher", image: "dispatcher:latest", env: [
    { name: "KARS_MISSION_IDENTITY_ROOT", valueFrom: { secretKeyRef: { name: dispatcherRootName, key: "root", optional: false } } },
  ] }] } };
  const deployment = { metadata: { ...meta(location.deployment, "deployment-uid"), labels: {
    "app.kubernetes.io/managed-by": "Helm", "app.kubernetes.io/component": "mission-dispatcher",
  }, annotations: { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": location.namespace,
    [`${p}mission-root-uid`]: "root-uid", "deployment.kubernetes.io/revision": "1" } as Record<string, string> },
  spec: { replicas: 1, strategy: { type: "Recreate" }, template }, status: { replicas: 1, updatedReplicas: 1, observedGeneration: 1 } };
  const rs = { metadata: { ...meta("dispatcher-rs", "rs-uid"), ownerReferences: [owner("Deployment", location.deployment, "deployment-uid")],
    labels: { "pod-template-hash": "abc" }, annotations: { "deployment.kubernetes.io/revision": "1" } },
  spec: { replicas: 1, template: structuredClone(template) }, status: { replicas: 1, observedGeneration: 1 } };
  rs.spec.template.metadata.labels["pod-template-hash"] = "abc";
  const pod = { metadata: { ...meta(location.podName, location.podUid), labels: { "pod-template-hash": "abc" },
    ownerReferences: [owner("ReplicaSet", "dispatcher-rs", "rs-uid")] }, spec: structuredClone(template.spec),
  status: { phase: "Running", conditions: [{ type: "Ready", status: "False" }] } };
  const secret = { metadata: { ...meta(dispatcherRootName, "root-uid"), ownerReferences: [owner("Deployment", location.deployment, "deployment-uid")], annotations: {
    [`${p}mission-root-version`]: "v1", [`${p}mission-owner-uid`]: "deployment-uid", [`${p}mission-namespace-uid`]: "namespace-uid", [`${p}mission-identity-role`]: "dispatcher",
  } }, immutable: true, type: "Opaque", data: { root: Buffer.from(root).toString("base64") } };
  const sets = { items: [rs], metadata: { continue: "" } };
  const pods = { items: [pod], metadata: { continue: "" } };
  const resources = new Map<string, unknown>([[nsPath, ns], [deploymentPath, deployment], [rsPath, rs], [podPath, pod], [secretPath, secret],
    [`/apis/apps/v1/namespaces/${location.namespace}/replicasets?limit=1000`, sets], [`${nsPath}/pods?limit=1000`, pods]]);
  const binding: DispatcherEntraBinding = {
    version: "v1", state: "ready", namespaceUid: "namespace-uid", deploymentUid: "deployment-uid",
    rootUid: "root-uid", journalUid: "entra-uid", ...entra,
    appId: "c3333333-3333-4333-8333-333333333333", objectId: "d4444444-4444-4444-8444-444444444444",
  };
  const entraPath = `${nsPath}/secrets/${dispatcherEntraName}`;
  const entraSecret = { metadata: { ...meta(dispatcherEntraName, "entra-uid"),
    ownerReferences: [owner("Deployment", location.deployment, "deployment-uid")] },
  immutable: true, type: "Opaque", data: { binding: Buffer.from(JSON.stringify(binding)).toString("base64") } };
  if (withEntra) {
    deployment.metadata.annotations[dispatcherEntraPin] = "entra-uid";
    resources.set(entraPath, entraSecret);
  }
  const calls: string[] = [];
  let before: ((path: string) => void) | undefined;
  const api: KubernetesJson = { async request<T>(method: string, path: string): Promise<T> {
    expect(method).toBe("GET"); calls.push(path); before?.(path);
    if (!resources.has(path)) throw new Error("Unexpected resource");
    return structuredClone(resources.get(path)) as T;
  } };
  return { api, calls, ns, deployment, rs, pod, secret, sets, pods, secretPath, deploymentPath, rsPath, podPath, nsPath,
    binding, entraPath, entraSecret,
    setBinding: (value: unknown) => { entraSecret.data.binding = Buffer.from(JSON.stringify(value)).toString("base64"); },
    onRead: (fn: (path: string) => void) => { before = fn; } };
}
type Fixture = ReturnType<typeof fixture>;

describe("dispatcher Entra binding custody", () => {
  it("returns only the immutable, pinned, owner-bound ready registration after reverse rechecks", async () => {
    const h = fixture(true);
    await expect(dispatcherCustody(h.api, location, root, entra)).resolves.toEqual({
      deploymentUid: "deployment-uid", podUid: location.podUid, namespaceUid: "namespace-uid", rootUid: "root-uid", entra: h.binding,
    });
    expect(h.calls.filter(path => path.includes("/secrets/"))).toEqual([h.secretPath, h.entraPath, h.entraPath, h.secretPath]);
    expect(h.calls.slice(-6)).toEqual([h.entraPath, h.secretPath, h.rsPath, h.podPath, h.deploymentPath, h.nsPath]);
  });
  it("never reads a registration Secret before its UID pin commits", async () => {
    const h = fixture(true); delete h.deployment.metadata.annotations[dispatcherEntraPin];
    await expect(dispatcherCustody(h.api, location, root, entra)).rejects.toThrow("custody");
    expect(h.calls).not.toContain(h.entraPath);
  });
  it("does not use an installed Entra binding in explicit development mode", async () => {
    const h = fixture(true);
    const result = await dispatcherCustody(h.api, location, root);
    expect(result).not.toHaveProperty("entra"); expect(h.calls).not.toContain(h.entraPath);
  });
  it.each<[string, (h: Fixture) => void]>([
    ["replaced Secret UID", h => { h.entraSecret.metadata.uid = "replacement"; }],
    ["unmatched pin", h => { h.deployment.metadata.annotations[dispatcherEntraPin] = "replacement"; }],
    ["wrong Secret name", h => { h.entraSecret.metadata.name = "another"; }],
    ["wrong namespace", h => { h.entraSecret.metadata.namespace = "another"; }],
    ["deleting Secret", h => { Reflect.set(h.entraSecret.metadata, "deletionTimestamp", "2026-10-06T00:00:00Z"); }],
    ["missing resourceVersion", h => { h.entraSecret.metadata.resourceVersion = ""; }],
    ["wrong owner UID", h => { h.entraSecret.metadata.ownerReferences[0].uid = "another"; }],
    ["wrong owner kind", h => { h.entraSecret.metadata.ownerReferences[0].kind = "ReplicaSet"; }],
    ["wrong owner name", h => { h.entraSecret.metadata.ownerReferences[0].name = "another"; }],
    ["non-controller owner", h => { h.entraSecret.metadata.ownerReferences[0].controller = false; }],
    ["unblocked deletion", h => { h.entraSecret.metadata.ownerReferences[0].blockOwnerDeletion = false; }],
    ["extra owner", h => { h.entraSecret.metadata.ownerReferences.push({ ...h.entraSecret.metadata.ownerReferences[0] }); }],
    ["mutable Secret", h => { h.entraSecret.immutable = false; }],
    ["wrong type", h => { h.entraSecret.type = "kubernetes.io/tls"; }],
    ["missing binding", h => { Reflect.deleteProperty(h.entraSecret.data, "binding"); }],
    ["extra key", h => { Reflect.set(h.entraSecret.data, "credential", "c2VjcmV0"); }],
    ["noncanonical base64", h => { h.entraSecret.data.binding += "\n"; }],
  ])("rejects %s without exposing binding material", async (_reason, mutate) => {
    const h = fixture(true); mutate(h);
    await expect(dispatcherCustody(h.api, location, root, entra)).rejects.toThrow(/^Dispatcher workload custody is not current$/);
  });
  it.each<[string, unknown]>([
    ["version", "v2"], ["state", "creating"], ["namespaceUid", "another"], ["deploymentUid", "another"],
    ["rootUid", "another"], ["journalUid", "another"],
    ["tenantId", "e5555555-5555-4555-8555-555555555555"], ["blueprintId", "e5555555-5555-4555-8555-555555555555"],
    ["appId", "C3333333-3333-4333-8333-333333333333"], ["objectId", "not-a-guid"], ["appId", 3], ["objectId", null],
  ])("rejects a mismatched or invalid binding %s (%#)", async (key, value) => {
    const h = fixture(true); h.setBinding({ ...h.binding, [key]: value });
    await expect(dispatcherCustody(h.api, location, root, entra)).rejects.toThrow(/^Dispatcher workload custody is not current$/);
  });
  it("requires exactly the ten registration fields, not extra credentials or missing fields", async () => {
    const h = fixture(true);
    h.setBinding({ ...h.binding, token: "private-token" });
    await expect(dispatcherCustody(h.api, location, root, entra)).rejects.toThrow("custody");
    for (const key of Object.keys(h.binding)) {
      const incomplete = { ...h.binding }; Reflect.deleteProperty(incomplete, key); h.setBinding(incomplete);
      await expect(dispatcherCustody(h.api, location, root, entra)).rejects.toThrow("custody");
    }
  });
  it.each([null, [], "not a binding", 1])("rejects non-object binding JSON %#", async value => {
    const h = fixture(true); h.setBinding(value);
    await expect(dispatcherCustody(h.api, location, root, entra)).rejects.toThrow("custody");
  });
  it("rejects invalid JSON without including its contents", async () => {
    const h = fixture(true); h.entraSecret.data.binding = Buffer.from("private-invalid-json").toString("base64");
    await expect(dispatcherCustody(h.api, location, root, entra)).rejects.toThrow(/^Invalid dispatcher Entra binding$/);
  });
  it.each(["entraSecret", "secret", "rs", "pod", "deployment", "ns"] as const)("rejects %s changes during Entra reverse collection", async resource => {
    const h = fixture(true);
    const path = ({ entraSecret: h.entraPath, secret: h.secretPath, rs: h.rsPath, pod: h.podPath, deployment: h.deploymentPath, ns: h.nsPath })[resource];
    h.onRead(current => { if (current === path && h.calls.filter(value => value === path).length === 2) h[resource].metadata.resourceVersion = "2"; });
    await expect(dispatcherCustody(h.api, location, root, entra)).rejects.toThrow("custody");
  });
});

describe("dispatcher bootstrap custody", () => {
  it("accepts a Running, not-yet-Ready Pod without a bootstrap deadlock", async () => {
    const h = fixture();
    await expect(dispatcherCustody(h.api, location, root)).resolves.toEqual({ deploymentUid: "deployment-uid", podUid: location.podUid, namespaceUid: "namespace-uid", rootUid: "root-uid" });
    expect(h.calls.filter(path => path.includes("/secrets/"))).toEqual([h.secretPath, h.secretPath]);
    expect(h.calls.slice(-5)).toEqual([h.secretPath, h.rsPath, h.podPath, h.deploymentPath, h.nsPath]);
  });
  it.each<[string, (h: Fixture) => void]>([
    ["uncommitted pin", h => { delete h.deployment.metadata.annotations[`${p}mission-root-uid`]; }],
    ["wrong Helm release", h => { h.deployment.metadata.annotations["meta.helm.sh/release-name"] = "other"; }],
    ["wrong component", h => { h.deployment.metadata.labels["app.kubernetes.io/component"] = "other"; }],
    ["rolling strategy", h => { h.deployment.spec.strategy.type = "RollingUpdate"; }],
    ["two replicas", h => { h.deployment.spec.replicas = 2; }],
    ["unobserved generation", h => { h.deployment.status.observedGeneration = 0; }],
    ["another Pod UID", h => { h.pod.metadata.uid = "other"; }],
    ["duplicate controller", h => { h.pod.metadata.ownerReferences.push({ ...h.pod.metadata.ownerReferences[0] }); }],
    ["wrong RS owner", h => { h.rs.metadata.ownerReferences[0].uid = "other"; }],
    ["stale revision", h => { h.rs.metadata.annotations["deployment.kubernetes.io/revision"] = "2"; }],
    ["template drift", h => { h.rs.spec.template.spec.containers[0].image = "other:latest"; }],
    ["optional root", h => { h.deployment.spec.template.spec.containers[0].env[0].valueFrom.secretKeyRef.optional = true; }],
    ["wrong actual root source", h => { h.pod.spec.containers[0].env[0].valueFrom.secretKeyRef.name = "other"; }],
    ["extra container", h => { h.pod.spec.containers.push(structuredClone(h.pod.spec.containers[0])); }],
    ["second owned Pod", h => { h.pods.items.push({ ...h.pod, metadata: { ...h.pod.metadata, uid: "second" } }); }],
    ["active older ReplicaSet", h => { h.sets.items.push({ ...h.rs, metadata: { ...h.rs.metadata, uid: "old" } }); }],
    ["truncated Pod list", h => { h.pods.metadata.continue = "more"; }],
    ["truncated RS list", h => { h.sets.metadata.continue = "more"; }],
  ])("rejects %s before reading a Secret", async (_reason, mutate) => {
    const h = fixture(); mutate(h);
    await expect(dispatcherCustody(h.api, location, root)).rejects.toThrow("custody");
    expect(h.calls.some(path => path.includes("/secrets/"))).toBe(false);
  });
  it.each<[string, (h: Fixture) => void]>([
    ["replaced root", h => { h.secret.metadata.uid = "new"; }],
    ["wrong owner", h => { h.secret.metadata.ownerReferences[0].uid = "other"; }],
    ["unblocked owner deletion", h => { h.secret.metadata.ownerReferences[0].blockOwnerDeletion = false; }],
    ["wrong namespace", h => { h.secret.metadata.annotations[`${p}mission-namespace-uid`] = "other"; }],
    ["wrong role", h => { h.secret.metadata.annotations[`${p}mission-identity-role`] = "runtime"; }],
    ["mutable Secret", h => { h.secret.immutable = false; }],
    ["wrong type", h => { h.secret.type = "kubernetes.io/tls"; }],
    ["extra key", h => { Reflect.set(h.secret.data, "extra", "AA=="); }],
    ["noncanonical encoding", h => { h.secret.data.root += "\n"; }],
    ["mounted mismatch", h => { h.secret.data.root = Buffer.from("b".repeat(64)).toString("base64"); }],
  ])("rejects %s without revealing root material", async (_reason, mutate) => {
    const h = fixture(); mutate(h);
    await expect(dispatcherCustody(h.api, location, root)).rejects.toThrow(/^Dispatcher workload custody is not current$/);
  });
  it.each(["secret", "rs", "pod", "deployment", "ns"] as const)("rejects %s mutation during reverse collection", async resource => {
    const h = fixture(); const path = ({ secret: h.secretPath, rs: h.rsPath, pod: h.podPath, deployment: h.deploymentPath, ns: h.nsPath })[resource];
    h.onRead(current => { if (current === path && h.calls.filter(value => value === path).length === 2) h[resource].metadata.resourceVersion = "2"; });
    await expect(dispatcherCustody(h.api, location, root)).rejects.toThrow("custody");
  });
});
