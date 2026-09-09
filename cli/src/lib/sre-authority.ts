// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { namespaceClaimed, type OwnershipObject } from "./namespace-ownership.js";

export type Execute = (file: string, args: readonly string[], options: { stdio: "pipe"; input?: string }) => Promise<{ stdout: string }>;
const RESOURCE = "karssreregistrations.kars.azure.com";
const CRD = "karssreregistrations.kars.azure.com";
const NS = "kars-sre";
const OLD_USER = `system:serviceaccount:${NS}:sandbox`;
const PRIVACY_REVISION = "kars.azure.com/sre-privacy/v2";

export interface ApiObject extends OwnershipObject {
  metadata: OwnershipObject["metadata"] & { generation?: number };
  spec?: Record<string, any>;
  status?: Record<string, any>;
  roleRef?: { apiGroup: string; kind: string; name: string };
  subjects?: Array<{ kind: string; name: string; namespace?: string; apiGroup?: string }>;
  rules?: Array<{ apiGroups?: string[]; resources?: string[]; verbs?: string[]; nonResourceURLs?: string[] }>;
}

export interface Enrollment {
  controller: { namespace: { name: string; uid: string }; deployment: { name: string; uid: string }; release: string };
  sandbox: { namespace: string; name: string; uid: string };
  runtimeNamespace: { name: string; uid: string };
  enabled: boolean;
  legacyBindings: Array<{
    kind: string; namespace?: string; name: string; uid: string; resourceVersion: string;
    roleRef: NonNullable<ApiObject["roleRef"]>; subjects: NonNullable<ApiObject["subjects"]>;
  }>;
  legacyConsumer?: { namespace: string; name: string; uid: string; resourceVersion: string };
}

export async function get(execute: Execute, kind: string, name: string, namespace?: string): Promise<ApiObject | undefined> {
  const { stdout } = await execute("kubectl", ["get", kind, name, ...(namespace ? ["-n", namespace] : []),
    "--ignore-not-found", "-o", "json"], { stdio: "pipe" });
  if (!stdout.trim()) return undefined;
  const object = JSON.parse(stdout) as ApiObject;
  if (!object.metadata?.uid || !object.metadata.resourceVersion || !object.metadata.name) {
    throw new Error("SRE authority API response omitted its exact identity");
  }
  return object;
}

async function list(execute: Execute, kind: string): Promise<ApiObject[]> {
  const { stdout } = await execute("kubectl", ["get", kind, "-A", "-o", "json"], { stdio: "pipe" });
  const result = JSON.parse(stdout) as { items?: ApiObject[] };
  if (!Array.isArray(result.items)) throw new Error("SRE authority inventory is malformed");
  for (const item of result.items) if (!item.metadata?.uid || !item.metadata.resourceVersion || !item.metadata.name) {
    throw new Error("SRE authority inventory omitted an object identity");
  }
  return result.items;
}

export function oldSubject(subject: NonNullable<ApiObject["subjects"]>[number]): boolean {
  return (subject.kind === "ServiceAccount" && subject.name === "sandbox" && subject.namespace === NS)
    || (subject.kind === "User" && subject.name === OLD_USER);
}

function oldGroup(subject: NonNullable<ApiObject["subjects"]>[number]): boolean {
  return subject.kind === "Group"
    && ["system:authenticated", "system:serviceaccounts", `system:serviceaccounts:${NS}`].includes(subject.name);
}

function dangerous(role: ApiObject): boolean {
  const includes = (items: string[] | undefined, value: string) => items?.some(item => item === "*" || item === value);
  return role.rules?.some(rule =>
    (includes(rule.apiGroups, "") && includes(rule.resources, "secrets")
      && ["get", "list", "watch"].some(verb => includes(rule.verbs, verb)))
    || (includes(rule.apiGroups, "") && ["pods/exec", "pods/proxy", "serviceaccounts/token"]
      .some(resource => includes(rule.resources, resource))
      && (includes(rule.verbs, "get") || includes(rule.verbs, "create")))
    || (includes(rule.apiGroups, "kars.azure.com") && includes(rule.resources, "karssreactions") && includes(rule.verbs, "create")),
  ) ?? false;
}

export async function legacyBindings(execute: Execute): Promise<ApiObject[]> {
  const bindings = [...await list(execute, "clusterrolebindings"), ...await list(execute, "rolebindings")];
  const result: ApiObject[] = [];
  for (const binding of bindings) {
    if (!binding.roleRef) throw new Error("RBAC binding omitted roleRef");
    const subjects = binding.subjects ?? [];
    if (subjects.some(oldGroup)) {
      const role = await get(execute, binding.roleRef.kind.toLowerCase(), binding.roleRef.name,
        binding.roleRef.kind === "Role" ? binding.metadata.namespace : undefined);
      if (!role) throw new Error("A referenced RBAC role is missing");
      if (dangerous(role)) throw new Error("A broad group grant gives the legacy SRE identity privileged access; restructure it explicitly before migration");
    }
    if (subjects.some(oldSubject)) {
      let ordinarySpawner = false;
      if (binding.roleRef.kind === "ClusterRole" && binding.roleRef.name === "kars-sandbox-spawner") {
        const role = await get(execute, "clusterrole", binding.roleRef.name);
        if (!role) throw new Error("A referenced spawner role is missing");
        ordinarySpawner = (role.rules ?? []).every(rule =>
          JSON.stringify(rule.apiGroups) === '["kars.azure.com"]'
          && JSON.stringify(rule.resources) === '["karssandboxes"]'
          && !rule.nonResourceURLs
          && (rule.verbs ?? []).every(verb => ["get", "list", "create", "delete"].includes(verb)));
      }
      if (!ordinarySpawner) result.push(binding);
    }
  }
  return result;
}

export async function requireRegistrar(execute: Execute): Promise<void> {
  const { stdout } = await execute("kubectl", ["auth", "can-i", "use", `${RESOURCE}/canonical`], { stdio: "pipe" });
  if (stdout.trim() !== "yes") throw new Error("Explicit cluster-level kars-sre-registrar permission is required");
}

export async function preview(execute: Execute, namespace: string, release: string): Promise<Enrollment> {
  const controllerNs = await get(execute, "namespace", namespace);
  const controller = await get(execute, "deployment", "kars-controller", namespace);
  const sandbox = await get(execute, "karssandbox", "sre", namespace);
  const runtime = await get(execute, "namespace", NS);
  if (!controllerNs || !controller || !sandbox || !runtime) throw new Error("Stage the unprivileged canonical SRE source and wait for its namespace claim before enrollment");
  if (controllerNs.metadata.deletionTimestamp || controller.metadata.deletionTimestamp
      || sandbox.metadata.deletionTimestamp || runtime.metadata.deletionTimestamp
      || !namespaceClaimed(runtime, sandbox)
      || sandbox.metadata.annotations?.["kars.azure.com/namespace-uid"] !== runtime.metadata.uid) {
    throw new Error("Canonical SRE source/runtime identity or claim is not live and complete");
  }
  const annotations = controller.metadata.annotations ?? {};
  if ((annotations["meta.helm.sh/release-name"] && annotations["meta.helm.sh/release-name"] !== release)
      || (annotations["meta.helm.sh/release-namespace"] && annotations["meta.helm.sh/release-namespace"] !== namespace)) {
    throw new Error("Controller belongs to another release");
  }
  const bindings = await legacyBindings(execute);
  const consumer = await get(execute, "deployment", "sre", NS);
  return {
    controller: {
      namespace: { name: namespace, uid: controllerNs.metadata.uid! },
      deployment: { name: "kars-controller", uid: controller.metadata.uid! }, release,
    },
    sandbox: { namespace, name: "sre", uid: sandbox.metadata.uid! },
    runtimeNamespace: { name: NS, uid: runtime.metadata.uid! },
    enabled: true,
    legacyBindings: bindings.map(binding => ({
      kind: binding.kind!, ...(binding.metadata.namespace ? { namespace: binding.metadata.namespace } : {}),
      name: binding.metadata.name!, uid: binding.metadata.uid!, resourceVersion: binding.metadata.resourceVersion!,
      roleRef: binding.roleRef!, subjects: binding.subjects ?? [],
    })),
    ...(consumer ? { legacyConsumer: { namespace: NS, name: "sre", uid: consumer.metadata.uid!,
      resourceVersion: consumer.metadata.resourceVersion! } } : {}),
  };
}

export interface Review {
  sandboxUid: string;
  namespaceUid: string;
  binding: string[];
  consumer?: string;
  registrationUid?: string;
  resourceVersion?: string;
}

export async function enroll(execute: Execute, spec: Enrollment, review: Review, dryRun: boolean): Promise<string> {
  await requireRegistrar(execute);
  if (spec.sandbox.uid !== review.sandboxUid || spec.runtimeNamespace.uid !== review.namespaceUid) {
    throw new Error("Reviewed SRE source/namespace UID no longer matches");
  }
  const expected = spec.legacyBindings.map(binding =>
    `${binding.kind}/${binding.namespace ?? ""}/${binding.name}=${binding.uid}@${binding.resourceVersion}`);
  if (expected.length !== review.binding.length || expected.some(value => !review.binding.includes(value))) {
    throw new Error(`Every legacy binding must be explicitly reviewed: ${expected.join(", ")}`);
  }
  const consumer = spec.legacyConsumer;
  if (consumer && review.consumer !== `${consumer.uid}@${consumer.resourceVersion}`) {
    throw new Error("Existing SRE consumer requires an exact --consumer UID@resourceVersion review");
  }
  const object = { apiVersion: "kars.azure.com/v1alpha1", kind: "KarsSRERegistration",
    metadata: { name: "canonical" }, spec };
  const existing = await get(execute, RESOURCE, "canonical");
  if (dryRun) return JSON.stringify(object, null, 2);
  if (existing) {
    if (existing.metadata.uid !== review.registrationUid || existing.metadata.resourceVersion !== review.resourceVersion) {
      throw new Error("Updating an enrollment requires its reviewed registration UID/resourceVersion");
    }
    // Enrollment is the complete reviewed spec, not an overlay on retired data.
    await execute("kubectl", ["patch", RESOURCE, "canonical", "--type=json", "-p", JSON.stringify([
      { op: "test", path: "/metadata/uid", value: review.registrationUid },
      { op: "test", path: "/metadata/resourceVersion", value: review.resourceVersion },
      { op: "replace", path: "/spec", value: spec },
    ])], { stdio: "pipe" });
  } else {
    if (review.registrationUid || review.resourceVersion) throw new Error("Reviewed registration disappeared");
    await execute("kubectl", ["create", "-f", "-", "-o", "json"], { stdio: "pipe", input: JSON.stringify(object) });
  }
  return "SRE enrollment recorded; the controller will perform the reviewed migration";
}

export async function registration(execute: Execute): Promise<ApiObject | undefined> {
  if (!await get(execute, "crd", CRD)) return undefined;
  return get(execute, RESOURCE, "canonical");
}

/** Read-only normal-deployment gate. Explicit authority staging is a separate
 * registrar operation; ordinary deploy paths must never silently retire grants. */
export async function assertSafeMutation(execute: Execute): Promise<void> {
  const reg = await registration(execute);
  if (!reg && !await get(execute, "namespace", NS)) return;
  const bindings = await legacyBindings(execute);
  if (bindings.length) throw new Error("Legacy SRE grants require 'kars sre authority stage/preview/enroll' before deployment changes");
  if (!reg) return;
  if (reg.spec?.enabled === false && reg.status?.phase === "Retired"
      && reg.status.observedGeneration === reg.metadata.generation) return;
  const spec = reg.spec as unknown as Enrollment;
  const current = await preview(execute, spec.controller.namespace.name, spec.controller.release);
  if (current.sandbox.uid !== spec.sandbox.uid || current.runtimeNamespace.uid !== spec.runtimeNamespace.uid
      || current.controller.deployment.uid !== spec.controller.deployment.uid
      || current.controller.namespace.uid !== spec.controller.namespace.uid
      || reg.status?.observedGeneration !== reg.metadata.generation
      || reg.status?.privacyRevision !== PRIVACY_REVISION
      || !["Ready", "Retired"].includes(reg.status?.phase ?? "")) {
    throw new Error("SRE authority is stale or migration is incomplete; deployment changes stopped");
  }
}

export async function assertRollbackSafe(execute: Execute): Promise<void> {
  if (await registration(execute)) {
    throw new Error("Rollback across enrolled SRE authority is unsafe; use a reviewed roll-forward release instead");
  }
  await assertSafeMutation(execute);
}

export async function waitForAuthority(execute: Execute, phase: "Ready" | "Retired", attempts = 90): Promise<void> {
  for (let attempt = 0; attempt < attempts; attempt++) {
    const reg = await registration(execute);
    if (reg?.status?.phase === phase && reg.status.observedGeneration === reg.metadata.generation
        && reg.status.privacyRevision === PRIVACY_REVISION) return;
    if (reg?.status?.phase === "Blocked" && reg.status.observedGeneration === reg.metadata.generation) {
      throw new Error(`SRE migration blocked: ${String(reg.status.detail ?? "inspect registration status")}`);
    }
    await new Promise(resolve => setTimeout(resolve, 2000));
  }
  throw new Error("SRE authority did not converge; verify the prerequisite controller/router images and registration status");
}

export async function retire(execute: Execute, uid: string, resourceVersion: string): Promise<void> {
  await requireRegistrar(execute);
  const reg = await registration(execute);
  if (!reg || reg.metadata.uid !== uid || reg.metadata.resourceVersion !== resourceVersion) {
    throw new Error("Reviewed registration UID/resourceVersion changed");
  }
  await execute("kubectl", ["patch", RESOURCE, "canonical", "--type=merge", "-p", JSON.stringify({
    metadata: { uid, resourceVersion }, spec: { enabled: false },
  })], { stdio: "pipe" });
  await waitForAuthority(execute, "Retired");
}

export async function assertDestroySafe(execute: Execute): Promise<void> {
  const reg = await registration(execute);
  if (!reg && !await get(execute, "namespace", NS)) return;
  if (reg && (reg.status?.phase !== "Retired" || reg.spec?.enabled !== false
      || reg.status.observedGeneration !== reg.metadata.generation)) {
    throw new Error("Retire the reviewed SRE registration before uninstalling or destroying its controller");
  }
  if ((await legacyBindings(execute)).length) throw new Error("Unretired legacy SRE bindings block uninstall/destroy");
}
