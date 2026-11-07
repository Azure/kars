// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { at, canonical, digest, PRIVATE_PREFIX, record, reviewed, type Execute } from "./private-activation.js";
import { schemaDocuments, type ObjectMap } from "./schema-documents.js";

export const HISTORICAL_HOOK_RETIREMENT = `${PRIVATE_PREFIX}historical-monitoring-hook-retirement`;
const prefix = "kars/charts/monitoring/templates/prometheus-operator/admission-webhooks/job-patch/";
const expected: Record<string, string> = {
  "serviceaccount.yaml": "ServiceAccount", "clusterrole.yaml": "ClusterRole", "clusterrolebinding.yaml": "ClusterRoleBinding",
  "role.yaml": "Role", "rolebinding.yaml": "RoleBinding", "job-createSecret.yaml": "Job", "job-patchWebhook.yaml": "Job",
};
const fail = "Historical Core failure requires unchanged successful successor hooks and absent hook workloads, accounts and RBAC; retain failed history";
export interface HistoricalCoreRelease {
  name: string; uid: string; revision: number; digest: string; object: ObjectMap; release: ObjectMap;
}
export interface HistoricalHookProof {
  version: 1;
  failed: { name: string; uid: string; revision: number; digest: string };
  successor: { name: string; uid: string; revision: number; digest: string };
  hooksDigest: string;
  namespace: string;
  account: string;
  resources: { kind: string; name: string; namespace?: string }[];
}
const identity = ({ name, uid, revision, digest: fingerprint }: HistoricalCoreRelease) => ({ name, uid, revision, digest: fingerprint });
const intent = (hook: ObjectMap) => { const copy = structuredClone(hook); delete copy.last_run; return copy; };

export function historicalHookProof(failed: HistoricalCoreRelease, history: HistoricalCoreRelease[]): HistoricalHookProof {
  const release = failed.release, hooks = release.hooks;
  if (!history.length || history.some((entry, index) => entry.revision !== index + 1)
    || !history.some(entry => canonical(identity(entry)) === canonical(identity(failed)))
    || release.info?.status !== "failed" || release.apply_method !== "ssa" || !Array.isArray(hooks) || hooks.length !== 8
    || failed.revision >= history.at(-1)!.revision) throw new Error(fail);
  const bootstrap = hooks.filter(hook => typeof hook.path === "string" && hook.path.startsWith(prefix));
  const policies = hooks.filter(hook => hook.path === "kars/templates/toolpolicy-default.yaml");
  if (bootstrap.length !== 7 || policies.length !== 1
    || canonical(bootstrap.map(hook => hook.path.slice(prefix.length)).sort()) !== canonical(Object.keys(expected).sort())
    || hooks.some(hook => !["", "Succeeded", "Failed"].includes(at(hook, "last_run", "phase") as string))) throw new Error(fail);
  const documents = bootstrap.map(hook => {
    const values = schemaDocuments(String(hook.manifest)), value = values[0];
    if (values.length !== 1 || value?.kind !== expected[hook.path.slice(prefix.length)] || hook.kind !== value.kind
      || hook.name !== value.metadata.name
      || value.apiVersion !== (value.kind === "Job" ? "batch/v1" : value.kind === "ServiceAccount" ? "v1" : "rbac.authorization.k8s.io/v1")) throw new Error(fail);
    return value;
  });
  const policy = schemaDocuments(String(policies[0].manifest));
  if (policy.length !== 1 || policy[0]?.apiVersion !== "kars.azure.com/v1alpha1" || policy[0].kind !== "ToolPolicy"
    || policy[0].metadata.name !== "kars-default" || policy[0].metadata.namespace !== release.namespace
    || policies[0].kind !== "ToolPolicy") throw new Error(fail);
  const account = documents.find(value => value.kind === "ServiceAccount")!;
  const namespace = account.metadata.namespace, accountName = account.metadata.name;
  if (typeof namespace !== "string" || !namespace || typeof accountName !== "string" || !accountName
    || documents.some(value => typeof value.metadata.name !== "string"
      || (!["ClusterRole", "ClusterRoleBinding"].includes(value.kind) && value.metadata.namespace !== namespace)
      || (["ClusterRole", "ClusterRoleBinding"].includes(value.kind) && value.metadata.namespace !== undefined)
      || (value.kind === "Job" && at(value, "spec", "template", "spec", "serviceAccountName") !== accountName))) throw new Error(fail);
  for (const document of documents) {
    if (["RoleBinding", "ClusterRoleBinding"].includes(document.kind)) {
      const roleKind = document.kind === "RoleBinding" ? "Role" : "ClusterRole";
      if (canonical(document.subjects) !== canonical([{ kind: "ServiceAccount", name: accountName, namespace }])
        || canonical(document.roleRef) !== canonical({ apiGroup: "rbac.authorization.k8s.io", kind: roleKind,
          name: documents.find(value => value.kind === roleKind)!.metadata.name })) throw new Error(fail);
    }
    if (document.kind === "ServiceAccount" && (document.secrets?.length || document.imagePullSecrets?.length)) throw new Error(fail);
    if (document.kind === "Job") {
      const spec = record(at(document, "spec", "template", "spec"));
      // This recovery covers service-account-only certgen jobs, not arbitrary
      // hooks carrying independent credentials that account deletion cannot revoke.
      if (Object.keys(spec).some(key => !["containers", "restartPolicy", "serviceAccountName", "securityContext",
        "nodeSelector", "affinity", "tolerations", "priorityClassName", "automountServiceAccountToken"].includes(key))
        || !Array.isArray(spec.containers) || spec.containers.length !== 1
        || spec.containers.some(value => {
          const container = record(value);
          return Object.keys(container).some(key => !["name", "image", "imagePullPolicy", "args", "securityContext", "resources"].includes(key))
            || typeof container.image !== "string" || !/:latest@sha256:[a-f0-9]{64}$/.test(container.image)
            || at(container, "securityContext", "allowPrivilegeEscalation") !== false
            || canonical(at(container, "securityContext", "capabilities")) !== canonical({ drop: ["ALL"] });
        })) throw new Error(fail);
    }
  }
  const intents = hooks.map(intent).sort((a, b) => a.path.localeCompare(b.path));
  const successor = history.find(entry => entry.revision > failed.revision && entry.release.apply_method === "ssa"
    && ["superseded", "deployed"].includes(entry.release.info?.status)
    && Array.isArray(entry.release.hooks) && entry.release.hooks.length === hooks.length
    && entry.release.hooks.every((hook: ObjectMap) => at(hook, "last_run", "phase") === "Succeeded")
    && canonical(entry.release.hooks.map(intent).sort((a: ObjectMap, b: ObjectMap) => a.path.localeCompare(b.path))) === canonical(intents));
  if (!successor) throw new Error(fail);
  return { version: 1, failed: identity(failed), successor: identity(successor), hooksDigest: digest(intents),
    namespace, account: accountName, resources: documents.map(value => ({ kind: value.kind, name: value.metadata.name,
      ...(value.metadata.namespace ? { namespace: value.metadata.namespace } : {}) })) };
}

export async function assertHistoricalHooksAbsent(run: Execute, proof: HistoricalHookProof): Promise<void> {
  // Deleted service accounts invalidate bound tokens; no terminal Job status alone
  // establishes that a hook can no longer write credentials or admission settings.
  for (const resource of proof.resources) {
    const output = await run(["get", resource.kind, resource.name, ...(resource.namespace ? ["-n", resource.namespace] : []),
      "--ignore-not-found", "-o", "json"]);
    if (output.trim()) throw new Error(fail);
  }
  const pods = record(JSON.parse(await run(["get", "pods", "-n", proof.namespace, "--chunk-size=0", "-o", "json"])));
  if (at(pods, "metadata", "continue") || !Array.isArray(pods.items) || pods.items.length > 512
    || pods.items.some(pod => typeof at(pod, "spec", "serviceAccountName") !== "string"
      || !at(pod, "spec", "serviceAccountName") || at(pod, "spec", "serviceAccountName") === proof.account)) throw new Error(fail);
}

export async function verifyHistoricalHookRetirement(run: Execute, entry: HistoricalCoreRelease, history: HistoricalCoreRelease[]): Promise<boolean> {
  const marker = at(entry.object, "metadata", "annotations", HISTORICAL_HOOK_RETIREMENT);
  if (marker === undefined) return false;
  const proof = historicalHookProof(entry, history);
  if (entry.object.immutable !== true || marker !== canonical(proof)) throw new Error(fail);
  await assertHistoricalHooksAbsent(run, proof);
  return true;
}

export async function freezeHistoricalHookFailure(run: Execute, entry: HistoricalCoreRelease, proof: HistoricalHookProof): Promise<void> {
  await assertHistoricalHooksAbsent(run, proof);
  const id = reviewed(entry.object), annotations = { ...record(at(entry.object, "metadata", "annotations") ?? {}),
    [HISTORICAL_HOOK_RETIREMENT]: canonical(proof) };
  const result = record(JSON.parse(await run(["patch", "secret", id.name, "-n", entry.object.metadata.namespace,
    "--type=json", "--patch-file=/dev/stdin", "-o", "json"], JSON.stringify([
    { op: "test", path: "/metadata/uid", value: id.uid },
    { op: "test", path: "/metadata/resourceVersion", value: id.resourceVersion },
    { op: "add", path: "/metadata/annotations", value: annotations }, { op: "add", path: "/immutable", value: true },
  ]))));
  if (reviewed(result).uid !== id.uid || reviewed(result).resourceVersion === id.resourceVersion || result.immutable !== true
    || at(result, "metadata", "annotations", HISTORICAL_HOOK_RETIREMENT) !== canonical(proof)
    || canonical(result.data) !== canonical(entry.object.data)) throw new Error(fail);
  await assertHistoricalHooksAbsent(run, proof);
}
