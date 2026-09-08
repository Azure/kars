// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

type Execute = (
  file: string, args: readonly string[], options: { stdio: "pipe"; input?: string },
) => Promise<{ stdout: string }>;

export const CLAIM = {
  version: "kars.azure.com/namespace-claim-version",
  namespace: "kars.azure.com/sandbox-namespace",
  name: "kars.azure.com/sandbox-name",
  uid: "kars.azure.com/sandbox-uid",
  namespaceUid: "kars.azure.com/namespace-uid",
  prestage: "kars.azure.com/namespace-prestage",
} as const;
const FINALIZER = "kars.azure.com/namespace-cleanup";
const MANAGER = "kars-controller/karssandbox";
const GUIDE = "docs/how-to/namespace-ownership.md";

export interface OwnershipObject {
  apiVersion?: string;
  kind?: string;
  metadata: {
    name?: string; namespace?: string; uid?: string; resourceVersion?: string;
    creationTimestamp?: string; deletionTimestamp?: string;
    annotations?: Record<string, string>; labels?: Record<string, string>;
    ownerReferences?: unknown[]; finalizers?: string[];
    managedFields?: Array<{
      manager?: string; operation?: string; subresource?: string;
      fieldsV1?: Record<string, unknown>;
    }>;
  };
  status?: { namespace?: string };
  spec?: {
    selector?: { matchLabels?: Record<string, string> };
    template?: { metadata?: { labels?: Record<string, string> } };
  };
}

function fail(message: string): never {
  throw new Error(`NamespaceOwnershipConflict: ${message}; see ${GUIDE}`);
}

function identity(object: OwnershipObject): void {
  if (!object.metadata?.uid || !object.metadata.resourceVersion || !object.metadata.name) {
    fail("API object omitted name/UID/resourceVersion");
  }
}

function validateTarget(name: string, namespace: string): void {
  for (const value of [name, namespace]) {
    if (!value || value.length > 63 || !/^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/.test(value)) {
      fail("target name/workspace is not a Kubernetes DNS label");
    }
  }
}

export function claimAnnotations(sandbox: OwnershipObject): Record<string, string> {
  identity(sandbox);
  if (!sandbox.metadata.namespace || !sandbox.metadata.name) fail("Sandbox identity is incomplete");
  return {
    [CLAIM.version]: "v1", [CLAIM.namespace]: sandbox.metadata.namespace,
    [CLAIM.name]: sandbox.metadata.name, [CLAIM.uid]: sandbox.metadata.uid!,
  };
}

/** Strict checks shared by diagnostics, explicit adoption, and credential prestaging. */
export function namespaceClaimed(namespace: OwnershipObject, sandbox: OwnershipObject): boolean {
  identity(namespace);
  identity(sandbox);
  if (namespace.metadata.name !== `kars-${sandbox.metadata.name}`
      || namespace.metadata.ownerReferences?.length) fail("namespace name or ownerReferences do not match");
  const bound = sandbox.metadata.annotations?.[CLAIM.namespaceUid];
  if (bound !== undefined && bound !== namespace.metadata.uid) fail("namespace was replaced");
  const annotations = namespace.metadata.annotations ?? {};
  const keys = [CLAIM.version, CLAIM.namespace, CLAIM.name, CLAIM.uid, CLAIM.prestage];
  if (!keys.some(key => annotations[key] !== undefined)) return false;
  for (const [key, value] of Object.entries(claimAnnotations(sandbox))) {
    if (annotations[key] !== value) fail("namespace is reserved for a different Sandbox UID/workspace");
  }
  if (annotations[CLAIM.prestage] !== undefined) fail("namespace is not bound to this Sandbox UID");
  return true;
}

export function namespacePrestaged(namespace: OwnershipObject, sandbox: OwnershipObject): boolean {
  const annotations = namespace.metadata.annotations ?? {};
  const nsTime = Date.parse(namespace.metadata.creationTimestamp ?? "");
  const sandboxTime = Date.parse(sandbox.metadata.creationTimestamp ?? "");
  return namespace.metadata.name === `kars-${sandbox.metadata.name}`
    && !namespace.metadata.ownerReferences?.length
    && annotations[CLAIM.version] === "v1"
    && annotations[CLAIM.namespace] === sandbox.metadata.namespace
    && annotations[CLAIM.name] === sandbox.metadata.name
    && annotations[CLAIM.uid] === undefined
    && annotations[CLAIM.prestage] === "bind-next-sandbox"
    && !!namespace.metadata.uid
    && sandbox.metadata.annotations?.[CLAIM.namespaceUid] === namespace.metadata.uid
    && Number.isFinite(nsTime) && Number.isFinite(sandboxTime) && nsTime <= sandboxTime;
}

function appliedField(object: OwnershipObject, path: string[]): boolean {
  return object.metadata.managedFields?.some(entry => {
    if (entry.manager !== MANAGER || entry.operation !== "Apply" || entry.subresource) return false;
    let value: unknown = entry.fieldsV1;
    for (const key of path) {
      if (!value || typeof value !== "object" || !(key in value)) return false;
      value = (value as Record<string, unknown>)[key];
    }
    return true;
  }) ?? false;
}

/** Keep this conjunctive test in sync with controller namespace_ownership::legacy_proof. */
export function legacyNamespaceProof(
  namespace: OwnershipObject, deployment: OwnershipObject, sandbox: OwnershipObject,
): boolean {
  const name = sandbox.metadata.name;
  const target = `kars-${name}`;
  const parent = deployment.metadata.labels?.["kars.azure.com/parent-namespace"];
  const deployed = Date.parse(deployment.metadata.creationTimestamp ?? "");
  const created = Date.parse(sandbox.metadata.creationTimestamp ?? "");
  return sandbox.status?.namespace === target
    && (sandbox.metadata.finalizers?.includes(FINALIZER) ?? false)
    && namespace.metadata.labels?.["kars.azure.com/sandbox"] === name
    && namespace.metadata.labels?.["kars.azure.com/role"] === "sandbox"
    && appliedField(namespace, ["f:metadata", "f:labels", "f:kars.azure.com/sandbox"])
    && deployment.metadata.name === name && deployment.metadata.namespace === target
    && !deployment.metadata.ownerReferences?.length && !deployment.metadata.deletionTimestamp
    && deployment.metadata.labels?.["kars.azure.com/sandbox"] === name
    && (parent === undefined || parent === sandbox.metadata.namespace)
    && appliedField(deployment, ["f:spec"])
    && Number.isFinite(deployed) && Number.isFinite(created) && deployed > created
    && deployment.spec?.selector?.matchLabels?.["kars.azure.com/sandbox"] === name
    && deployment.spec?.template?.metadata?.labels?.["kars.azure.com/sandbox"] === name;
}

async function get(execute: Execute, kind: string, name: string, namespace?: string): Promise<OwnershipObject | undefined> {
  // --ignore-not-found suppresses only 404. Auth, transport, and parsing errors
  // must stop the operation; they are never interpreted as an empty cluster.
  const { stdout } = await execute("kubectl", [
    "get", kind, name, ...(namespace ? ["-n", namespace] : []),
    "--ignore-not-found", "--show-managed-fields=true", "-o", "json",
  ], { stdio: "pipe" });
  if (!String(stdout).trim()) return undefined;
  const object = JSON.parse(String(stdout)) as OwnershipObject;
  identity(object);
  return object;
}

async function sandboxes(execute: Execute): Promise<OwnershipObject[]> {
  const { stdout } = await execute("kubectl", [
    "get", "karssandboxes", "-A", "--show-managed-fields=true", "-o", "json",
  ], { stdio: "pipe" });
  const list = JSON.parse(String(stdout)) as { items?: OwnershipObject[] };
  if (!Array.isArray(list.items)) fail("Sandbox inventory is incomplete");
  for (const sandbox of list.items) {
    identity(sandbox);
    if (!sandbox.metadata.namespace) fail("Sandbox inventory omitted its workspace namespace");
  }
  return list.items;
}

export async function inspectNamespaceOwnership(execute: Execute): Promise<string[]> {
  const inventory = await sandboxes(execute);
  const diagnostics: string[] = [];
  for (const sandbox of inventory) {
    const name = sandbox.metadata.name!;
    const label = `${sandbox.metadata.namespace}/${name}`;
    const target = await get(execute, "namespace", `kars-${name}`);
    const unique = inventory.filter(s => s.metadata.name === name).length === 1;
    if (!target) {
      if (!unique) fail(`${label}: same-name Sandboxes exist in different workspaces`);
      if (sandbox.metadata.annotations?.[CLAIM.namespaceUid] && !sandbox.metadata.deletionTimestamp) {
        fail(`${label}: previously bound namespace is missing`);
      }
      diagnostics.push(`${label}: ${sandbox.metadata.deletionTimestamp ? "cleanup pending" : "new namespace (atomic create)"}`);
      continue;
    }
    if (target.metadata.deletionTimestamp && !sandbox.metadata.deletionTimestamp) fail(`${label}: namespace is terminating`);
    if (namespacePrestaged(target, sandbox)) {
      if (!unique) fail(`${label}: prestage target is ambiguous`);
      diagnostics.push(`${label}: explicit prestage (will bind UID)`);
      continue;
    }
    if (namespaceClaimed(target, sandbox)) {
      diagnostics.push(`${label}: namespace claim v1 verified`);
      continue;
    }
    if (!unique) fail(`${label}: legacy namespace has multiple possible owners`);
    const deployment = await get(execute, "deployment", name, `kars-${name}`);
    if (!deployment || !legacyNamespaceProof(target, deployment, sandbox)) {
      fail(`${label}: legacy ownership is unproven; resources preserved; explicit adoption required`);
    }
    diagnostics.push(`${label}: unambiguous legacy deployment (metadata-only adoption)`);
  }
  return diagnostics;
}

/** Explicit administrator decision, never a preflight side effect or a force takeover. */
export async function adoptNamespace(
  execute: Execute, name: string, sourceNamespace: string, sandboxUid: string, namespaceUid: string,
): Promise<void> {
  validateTarget(name, sourceNamespace);
  const inventory = await sandboxes(execute);
  const candidates = inventory.filter(s => s.metadata.name === name);
  const sandbox = candidates[0];
  if (candidates.length !== 1 || sandbox.metadata.namespace !== sourceNamespace
      || sandbox.metadata.uid !== sandboxUid || sandbox.metadata.deletionTimestamp) {
    fail("reviewed Sandbox UID/workspace is no longer the unique live owner");
  }
  const namespace = await get(execute, "namespace", `kars-${name}`);
  if (!namespace || namespace.metadata.uid !== namespaceUid || namespace.metadata.deletionTimestamp) {
    fail("reviewed namespace UID is missing, replaced, or terminating");
  }
  // A partial/foreign claim is not an invitation to overwrite it.
  namespaceClaimed(namespace, sandbox);
  await execute("kubectl", ["patch", "namespace", `kars-${name}`, "--type=merge", "-p",
    JSON.stringify({ metadata: {
      uid: namespaceUid, resourceVersion: namespace.metadata.resourceVersion,
      annotations: claimAnnotations(sandbox),
    } }),
  ], { stdio: "pipe" });
}

/** Preserve add's pre-CR credential staging, with an explicit two-way namespace
 * reservation. Returning the namespace UID lets add include it in the CR CREATE.
 * Interrupted explicit reservations can be resumed, but unmarked namespaces
 * are never claimed merely because their name matches. */
export async function prepareCredentialNamespace(
  execute: Execute, name: string, sourceNamespace: string,
): Promise<string> {
  validateTarget(name, sourceNamespace);
  const inventory = await sandboxes(execute);
  const candidates = inventory.filter(s => s.metadata.name === name);
  if (candidates.some(s => s.metadata.namespace !== sourceNamespace)) fail("sandbox name belongs to another workspace");
  const sandbox = candidates[0];
  const existing = await get(execute, "namespace", `kars-${name}`);
  if (existing) {
    const annotations = existing.metadata.annotations ?? {};
    if (!sandbox && !existing.metadata.deletionTimestamp && !existing.metadata.ownerReferences?.length
        && annotations[CLAIM.version] === "v1"
        && annotations[CLAIM.namespace] === sourceNamespace && annotations[CLAIM.name] === name
        && annotations[CLAIM.uid] === undefined && annotations[CLAIM.prestage] === "bind-next-sandbox") {
      return existing.metadata.uid!;
    }
    if (!sandbox || existing.metadata.deletionTimestamp) fail("existing namespace needs explicit adoption before credential staging");
    if (!namespaceClaimed(existing, sandbox)) {
      const deployment = await get(execute, "deployment", name, `kars-${name}`);
      if (!deployment || !legacyNamespaceProof(existing, deployment, sandbox)) fail("credential target ownership is unproven");
    }
    if (sandbox.metadata.deletionTimestamp) fail("Sandbox is terminating");
    return existing.metadata.uid!;
  }
  if (sandbox) fail("existing Sandbox has no namespace; wait for the controller");
  const { stdout } = await execute("kubectl", ["create", "-f", "-", "-o", "json"], {
    input: JSON.stringify({
      apiVersion: "v1", kind: "Namespace", metadata: {
        name: `kars-${name}`,
        annotations: {
          [CLAIM.version]: "v1", [CLAIM.namespace]: sourceNamespace,
          [CLAIM.name]: name, [CLAIM.prestage]: "bind-next-sandbox",
        },
        labels: {
          "app.kubernetes.io/name": "kars", "app.kubernetes.io/component": "sandbox",
          "kars.azure.com/sandbox": name, "kars.azure.com/role": "sandbox",
          "kars.azure.com/isolated": "strict",
          "pod-security.kubernetes.io/enforce": "privileged",
          "pod-security.kubernetes.io/audit": "baseline",
          "pod-security.kubernetes.io/warn": "baseline",
        },
      },
    }), stdio: "pipe",
  });
  const created = JSON.parse(String(stdout)) as OwnershipObject;
  identity(created);
  return created.metadata.uid!;
}
