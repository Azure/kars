// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash } from "node:crypto";
import {
  at, canonical, digest, implicitMemoryPressureToleration, kinds, read, record, reviewed, template, templateDigest,
  type Execute, type Json, type NamespaceReview, type PrivateActivation,
} from "./private-activation.js";
import { PrivateCommandFailure, privateCommandFailure } from "./private-activation-command-diagnostics.js";

type ObjectValue = ReturnType<typeof record>;
const wi = "azure.workload.identity/";
const tokenName = "azure-identity-token";
const tokenDirectory = "/var/run/secrets/azure/tokens";
const apiDirectory = "/var/run/secrets/kubernetes.io/serviceaccount";
const uuid = /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/i;
const topology = ["topology.kubernetes.io/region", "topology.kubernetes.io/zone"];
const revisionLabel = "kars.azure.com/observation-privacy-revision";
const controllerUid = "kars.azure.com/privacy-controller-uid";
const namespaceUid = "kars.azure.com/privacy-namespace-uid";
const failure = "Consumer execution differs from the reviewed controller template; Pod admission could not be verified; preserve the original review and consumer";

function array(value: unknown): Json[] {
  if (!Array.isArray(value)) throw new Error(failure);
  return value as Json[];
}

function metadata(value: unknown): ObjectValue {
  const meta = record(at(value, "metadata") ?? {});
  return structuredClone({ labels: meta.labels ?? {}, annotations: meta.annotations ?? {}, finalizers: meta.finalizers ?? [] });
}

function normalized(value: Json): Json {
  if (Array.isArray(value)) return value.map(normalized);
  if (value && typeof value === "object") {
    return Object.fromEntries(Object.entries(value).filter(([, item]) =>
      !(Array.isArray(item) && item.length === 0)).map(([key, item]) => [key, normalized(item)]));
  }
  return value;
}

function specKey(value: unknown, scheduled: boolean): string {
  const spec = structuredClone(record(value));
  if (scheduled) delete spec.nodeName;
  const automatic = array(spec.volumes ?? []).filter(v => String(at(v, "name")).startsWith("kube-api-access-"));
  if (automatic.length > 1) throw new Error(failure);
  const name = automatic[0] && at(automatic[0], "name");
  if (name) {
    record(automatic[0]).name = "kube-api-access-reviewed";
    for (const kind of ["containers", "initContainers", "ephemeralContainers"]) {
      for (const container of array(spec[kind] ?? [])) {
        for (const mount of array(at(container, "volumeMounts") ?? [])) {
          if (at(mount, "name") === name) record(mount).name = "kube-api-access-reviewed";
        }
      }
    }
  }
  if (spec.tolerations) spec.tolerations = array(spec.tolerations).sort((a, b) => canonical(a).localeCompare(canonical(b)));
  return canonical(normalized(spec));
}

function appendMount(spec: ObjectValue, mount: ObjectValue): void {
  for (const kind of ["containers", "initContainers"]) {
    for (const container of array(spec[kind] ?? [])) {
      const value = record(container);
      const mounts = array(value.volumeMounts ?? []);
      if (mounts.some(m => at(m, "name") === mount.name || at(m, "mountPath") === mount.mountPath)) throw new Error(failure);
      value.volumeMounts = [...mounts, structuredClone(mount)];
    }
  }
}

async function publicationMetadata(
  execute: Execute, actual: ObjectValue, parent: ObjectValue, root: PrivateActivation["root"],
): Promise<{ value: ObjectValue; snapshots: [string, ObjectValue, string][] }> {
  const value = structuredClone(actual);
  const labels = record(value.labels), annotations = record(value.annotations);
  const snapshots: [string, ObjectValue, string][] = [];
  if (labels[revisionLabel] === undefined && annotations[controllerUid] === undefined && annotations[namespaceUid] === undefined) {
    return { value, snapshots };
  }
  const enabled = array(at(parent, "spec", "containers")).filter(c => at(c, "name") === "controller")
    .flatMap(c => array(at(c, "env") ?? [])).filter(e => at(e, "name") === "KARS_OBSERVATION_PRIVACY_RPC_ENABLED");
  if (enabled.length !== 1 || canonical(enabled[0]) !== canonical({ name: "KARS_OBSERVATION_PRIVACY_RPC_ENABLED", value: "true" })
    || at(parent, "metadata", "labels", revisionLabel) !== undefined
    || at(parent, "metadata", "annotations", controllerUid) !== undefined
    || at(parent, "metadata", "annotations", namespaceUid) !== undefined
    || annotations[controllerUid] !== root.account.uid || annotations[namespaceUid] !== root.namespace.uid) throw new Error(failure);
  if (labels[revisionLabel] !== undefined) {
    const descriptor = await read(execute, "configmap", "kars-observation-privacy", root.namespace.name);
    const service = await read(execute, "service", "kars-observation-privacy", root.namespace.name);
    const raw = at(descriptor, "data", "config.json");
    if (typeof raw !== "string" || Buffer.byteLength(raw) > 32768) throw new Error(failure);
    const endpoint = record(JSON.parse(raw));
    const fields = ["capability", "namespace", "namespaceUid", "controllerUid", "serviceUid", "port",
      "descriptorUid", "tlsUid", "tlsVersion", "serverName", "caPem", "expiresAt"];
    // Verify the existing publication marker, not a new RPC capability. The
    // publisher hashes its serialized descriptor bytes (not canonical JSON).
    if (canonical(Object.keys(endpoint).sort()) !== canonical(fields.sort())
      || fields.filter(key => !["port", "expiresAt"].includes(key)).some(key => typeof endpoint[key] !== "string" || !endpoint[key])
      || !Number.isInteger(endpoint.port) || Number(endpoint.port) < 1024 || Number(endpoint.port) > 65535
      || !Number.isSafeInteger(endpoint.expiresAt) || Number(endpoint.expiresAt) <= Date.now() / 1000
      || endpoint.serverName !== `privacy-${root.namespace.uid}.kars.internal`
      || typeof endpoint.caPem !== "string" || !endpoint.caPem.startsWith("-----BEGIN CERTIFICATE-----") || endpoint.caPem.length > 8192
      || endpoint.capability !== "kars.azure.com/observation-privacy/v1"
      || endpoint.namespace !== root.namespace.name || endpoint.namespaceUid !== root.namespace.uid
      || endpoint.controllerUid !== root.account.uid || endpoint.descriptorUid !== reviewed(descriptor).uid
      || endpoint.serviceUid !== reviewed(service).uid
      || ![descriptor, service].every(object => at(object, "metadata", "namespace") === root.namespace.name)
      || at(descriptor, "metadata", "annotations", controllerUid) !== root.account.uid
      || at(descriptor, "metadata", "annotations", namespaceUid) !== root.namespace.uid
      || labels[revisionLabel] !== createHash("sha256").update(raw).digest("hex").slice(0, 32)
      || canonical(at(service, "spec", "selector")) !== canonical({
        "app.kubernetes.io/name": "kars", "app.kubernetes.io/component": "controller", [revisionLabel]: labels[revisionLabel],
      })) throw new Error(failure);
    snapshots.push(["configmap", descriptor, root.namespace.name], ["service", service, root.namespace.name]);
    delete labels[revisionLabel];
  }
  delete annotations[controllerUid];
  delete annotations[namespaceUid];
  return { value, snapshots };
}

function withoutAddedTopology(meta: ObjectValue, parent: ObjectValue): ObjectValue {
  const result = structuredClone(meta);
  for (const key of topology) {
    if (at(parent, "labels", key) === undefined) delete record(result.labels)[key];
  }
  return result;
}

// This is a ceiling on admission authority, not a list of fields to discard.
// The API result must equal this derivation AND the complete live execution.
function supportedSpec(parent: ObjectValue, admitted: ObjectValue, account: ObjectValue): ObjectValue {
  const spec = structuredClone(record(parent.spec));
  spec.serviceAccountName ??= "default";
  spec.serviceAccount ??= spec.serviceAccountName;
  spec.enableServiceLinks ??= true;
  if (!spec.priorityClassName) {
    spec.priority ??= 0;
    spec.preemptionPolicy ??= "PreemptLowerPriority";
  }
  if (!array(spec.imagePullSecrets ?? []).length) spec.imagePullSecrets = structuredClone(account.imagePullSecrets ?? []);
  const volumes = array(spec.volumes ?? []);
  if (volumes.some(v => String(at(v, "name")).startsWith("kube-api-access-") || at(v, "name") === tokenName)) {
    throw new Error(failure);
  }
  spec.volumes = volumes;
  const automount = spec.automountServiceAccountToken ?? account.automountServiceAccountToken ?? true;
  if (automount === true) {
    const automatic = array(admitted.volumes ?? []).filter(v => String(at(v, "name")).startsWith("kube-api-access-"));
    if (automatic.length !== 1 || !/^kube-api-access-[a-z0-9]{5}$/.test(String(at(automatic[0], "name")))) throw new Error(failure);
    const name = at(automatic[0], "name")!;
    volumes.push({ name, projected: { defaultMode: 420, sources: [
      { serviceAccountToken: { expirationSeconds: 3607, path: "token" } },
      { configMap: { name: "kube-root-ca.crt", items: [{ key: "ca.crt", path: "ca.crt" }] } },
      { downwardAPI: { items: [{ path: "namespace", fieldRef: { apiVersion: "v1", fieldPath: "metadata.namespace" } }] } },
    ] } });
    appendMount(spec, { name, readOnly: true, mountPath: apiDirectory });
  }
  if (at(parent, "metadata", "labels", `${wi}use`) === "true") {
    const annotations = record(at(parent, "metadata", "annotations") ?? {});
    if (Object.keys(annotations).some(key => key.startsWith(wi) && key !== `${wi}service-account-token-expiration`)) {
      throw new Error(failure);
    }
    const expiry = Number(annotations[`${wi}service-account-token-expiration`]
      ?? at(account, "metadata", "annotations", `${wi}service-account-token-expiration`) ?? 3600);
    if (!Number.isInteger(expiry) || expiry < 3600 || expiry > 86400) throw new Error(failure);
    const client = at(account, "metadata", "annotations", `${wi}client-id`) ?? "";
    if (typeof client !== "string" || (client !== "" && !uuid.test(client))) throw new Error(failure);
    const first = array(admitted.containers)[0];
    const env = array(at(first, "env") ?? []);
    const literal = (name: string): string => {
      const entries = env.filter(e => at(e, "name") === name);
      if (entries.length !== 1 || typeof at(entries[0], "value") !== "string"
        || at(entries[0], "valueFrom") !== undefined) throw new Error(failure);
      return at(entries[0], "value") as string;
    };
    const tenant = at(account, "metadata", "annotations", `${wi}tenant-id`) ?? literal("AZURE_TENANT_ID");
    if (typeof tenant !== "string" || !uuid.test(tenant)) throw new Error(failure);
    const authority = literal("AZURE_AUTHORITY_HOST");
    if (!["https://login.microsoftonline.com/", "https://login.microsoftonline.us/", "https://login.chinacloudapi.cn/"].includes(authority)) {
      throw new Error(failure);
    }
    const additions = [
      { name: "AZURE_CLIENT_ID", value: client }, { name: "AZURE_TENANT_ID", value: tenant },
      { name: "AZURE_FEDERATED_TOKEN_FILE", value: `${tokenDirectory}/${tokenName}` },
      { name: "AZURE_AUTHORITY_HOST", value: authority },
    ];
    for (const kind of ["containers", "initContainers"]) {
      for (const container of array(spec[kind] ?? [])) {
        const value = record(container);
        const entries = array(value.env ?? []);
        value.env = [...entries, ...additions.filter(e => e.value !== "" && !entries.some(existing => at(existing, "name") === e.name))];
      }
    }
    volumes.push({ name: tokenName, projected: { defaultMode: 420, sources: [
      { serviceAccountToken: { audience: "api://AzureADTokenExchange", expirationSeconds: expiry, path: tokenName } },
    ] } });
    appendMount(spec, { name: tokenName, readOnly: true, mountPath: tokenDirectory });
  }
  const tolerations = array(spec.tolerations ?? []);
  for (const key of ["node.kubernetes.io/not-ready", "node.kubernetes.io/unreachable"]) {
    if (!tolerations.some(t => [key, ""].includes(String(at(t, "key") ?? ""))
      && ["NoExecute", ""].includes(String(at(t, "effect") ?? "")))) {
      tolerations.push({ key, operator: "Exists", effect: "NoExecute", tolerationSeconds: 300 });
    }
  }
  const memory = implicitMemoryPressureToleration(spec);
  if (memory && !tolerations.some(t => canonical(t) === canonical(memory))) tolerations.push(memory);
  spec.tolerations = tolerations;
  return spec;
}

export async function verifyRootPodAdmission(
  execute: Execute, chain: ObjectValue[], scope: NamespaceReview, root: PrivateActivation["root"],
): Promise<void> {
  let check: "identity" | "metadata" | "ownership" | "inputs" | "execution" | "response" | "snapshots" = "identity";
  try {
    const [pod, replicaSet, deployment] = chain;
    if (chain.length !== 3 || pod?.kind !== "Pod" || replicaSet?.kind !== "ReplicaSet" || deployment?.kind !== "Deployment"
      || reviewed(deployment).uid !== root.deployment.uid || reviewed(deployment).name !== root.deployment.name
      || templateDigest(deployment) !== root.templateDigest || scope.namespace.uid !== root.namespace.uid
      || scope.namespace.name !== root.namespace.name
      || chain.some(value => at(value, "metadata", "namespace") !== scope.namespace.name)
      || at(pod, "spec", "serviceAccountName") !== root.account.name
      || at(replicaSet, "spec", "template", "spec", "serviceAccountName") !== root.account.name) throw new Error(failure);
    check = "metadata";
    const parent = template(replicaSet);
    const publication = await publicationMetadata(execute, metadata(pod), parent, root);
    if (canonical(withoutAddedTopology(publication.value, metadata(parent))) !== canonical(metadata(parent))) throw new Error(failure);
    const rsMeta = metadata(parent);
    const rootMeta = metadata(template(deployment));
    const hash = at(rsMeta, "labels", "pod-template-hash");
    if (hash !== undefined) {
      if (typeof hash !== "string" || reviewed(replicaSet).name !== `${reviewed(deployment).name}-${hash}`
        || at(replicaSet, "spec", "selector", "matchLabels", "pod-template-hash") !== hash) throw new Error(failure);
      delete record(rsMeta.labels)["pod-template-hash"];
    }
    if (canonical(rsMeta) !== canonical(rootMeta)
      || canonical(parent.spec) !== canonical(template(deployment).spec)) throw new Error(failure);
    check = "ownership";
    const owners = array(at(pod, "metadata", "ownerReferences"));
    const ownerReference = (value: ObjectValue) => [{
      apiVersion: "apps/v1", kind: value.kind, name: reviewed(value).name, uid: reviewed(value).uid,
      controller: true, blockOwnerDeletion: true,
    }];
    if (canonical(owners) !== canonical(ownerReference(replicaSet))
      || canonical(at(replicaSet, "metadata", "ownerReferences")) !== canonical(ownerReference(deployment))) throw new Error(failure);
    check = "inputs";
    const ns = await read(execute, "namespace", scope.namespace.name);
    const account = await read(execute, "serviceaccount", root.account.name, scope.namespace.name);
    if (reviewed(ns).uid !== scope.namespace.uid || reviewed(account).uid !== root.account.uid
      || at(account, "metadata", "namespace") !== scope.namespace.name) throw new Error(failure);
    check = "execution";
    const scheduled = at(parent, "spec", "nodeName") === undefined;
    if (specKey(supportedSpec(parent, record(pod.spec), account), false) !== specKey(pod.spec, scheduled)) throw new Error(failure);
    const meta = record(parent.metadata ?? {});
    if (Object.keys(meta).some(key => !["labels", "annotations", "finalizers", "creationTimestamp"].includes(key))) throw new Error(failure);
    const name = `kars-private-review-${digest(chain.map(value => reviewed(value, true))).slice(0, 32)}`;
    const request = { apiVersion: "v1", kind: "Pod", metadata: {
      ...structuredClone(meta), name, namespace: scope.namespace.name, ownerReferences: structuredClone(owners),
    }, spec: structuredClone(record(parent.spec)) };
    // Replay topology admission for the already-bound Pod without reading Nodes
    // or scheduling anything. Its nodeName and complete Pod snapshot stay fenced.
    if (scheduled && at(pod, "spec", "nodeName") !== undefined) request.spec.nodeName = at(pod, "spec", "nodeName")!;
    // Raw POST preserves unknown runtime fields; Strict rejects pruning. DryRun=All
    // is unconditional and never retried as a real create or an UPDATE.
    const args = ["create", "--raw",
      `/api/v1/namespaces/${encodeURIComponent(scope.namespace.name)}/pods?dryRun=All&fieldValidation=Strict`,
      "--request-timeout=30s", "-f", "-"];
    let response: ObjectValue;
    try {
      response = record(JSON.parse(await execute(args, JSON.stringify(request))));
    } catch (error) {
      throw privateCommandFailure(error, args, "Review");
    }
    check = "response";
    if (response.apiVersion !== "v1" || response.kind !== "Pod" || at(response, "metadata", "name") !== name
      || at(response, "metadata", "namespace") !== scope.namespace.name || at(response, "metadata", "resourceVersion") !== undefined
      || typeof at(response, "metadata", "uid") !== "string" || !at(response, "metadata", "uid")
      || at(response, "metadata", "uid") === reviewed(pod, true).uid || at(response, "metadata", "deletionTimestamp") != null
      || canonical(withoutAddedTopology(metadata(response), metadata(request))) !== canonical(metadata(request))
      || canonical(metadata(response)) !== canonical(publication.value)
      || canonical(at(response, "metadata", "ownerReferences")) !== canonical(owners)) throw new Error(failure);
    const spec = record(response.spec);
    if (canonical(spec.nodeName ?? null) !== canonical(request.spec.nodeName ?? null)
      || specKey(supportedSpec(parent, spec, account), false) !== specKey(spec, scheduled)
      || specKey(pod.spec, scheduled) !== specKey(spec, scheduled)) throw new Error(failure);
    check = "snapshots";
    for (const [kind, before, namespace] of [
      ["namespace", ns, undefined], ["serviceaccount", account, scope.namespace.name],
      ...chain.map(value => [kinds[String(value.kind)]!, value, scope.namespace.name] as const),
      ...publication.snapshots,
    ] as const) {
      const after = kind === "pods"
        ? record(JSON.parse(await execute(["get", kind, reviewed(before, true).name, "-n", scope.namespace.name, "-o", "json"])))
        : await read(execute, kind, reviewed(before, true).name, namespace);
      if (canonical(reviewed(after, kind === "pods")) !== canonical(reviewed(before, true))
        || canonical(after.metadata) !== canonical(before.metadata)
        || canonical(after.spec ?? null) !== canonical(before.spec ?? null)
        || (["serviceaccount", "configmap", "service"].includes(kind) && canonical(after) !== canonical(before))) throw new Error(failure);
    }
  } catch (error) {
    if (error instanceof PrivateCommandFailure) throw error;
    throw new Error(`${failure} (check: ${check})`);
  }
}
