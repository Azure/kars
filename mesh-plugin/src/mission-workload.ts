// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { isDeepStrictEqual } from "node:util";

export interface MissionMetadata {
  name: string; namespace?: string; uid?: string; resourceVersion?: string; generation?: number;
  deletionTimestamp?: string; annotations?: Record<string, string>; labels?: Record<string, string>;
  ownerReferences?: Array<{ apiVersion: string; kind: string; name: string; uid: string; controller?: boolean }>;
  managedFields?: Array<{ manager?: string; operation?: string; fieldsV1?: Record<string, unknown> }>;
}
export interface MissionWorkloadBinding {
  sandboxName: string; sandboxUid: string; namespaceUid: string;
  deploymentUid: string; deploymentGeneration: number; replicaSetName: string; replicaSetUid: string;
  podName: string; podUid: string;
}
interface PodTemplate {
  metadata?: { labels?: Record<string, string>; [key: string]: unknown };
  spec?: { containers?: Array<{ name: string; [key: string]: unknown }>; [key: string]: unknown };
}
interface Workload {
  metadata: MissionMetadata;
  spec?: { replicas?: number; paused?: boolean; template?: PodTemplate };
  status?: { observedGeneration?: number; replicas?: number; readyReplicas?: number;
    availableReplicas?: number; updatedReplicas?: number; terminatingReplicas?: number };
}
export interface MissionWorkloadResources {
  namespace: { metadata: MissionMetadata; status?: { phase?: string } };
  sandbox: { metadata: MissionMetadata; spec?: { suspended?: boolean } };
  deployment: Workload;
  replicaSet: Workload;
  pod: { metadata: MissionMetadata; status?: { phase?: string; conditions?: Array<{ type: string; status: string }> } };
}
const prefix = "kars.azure.com/";
const hashLabel = "pod-template-hash";
const revisionKey = "deployment.kubernetes.io/revision";

export function singleControllerOwner(meta: MissionMetadata, apiVersion: string, kind: string, name: string, uid: string): boolean {
  const owners = meta.ownerReferences?.filter(o => o.controller === true) ?? [];
  return owners.length === 1 && owners[0].apiVersion === apiVersion && owners[0].kind === kind
    && owners[0].name === name && owners[0].uid === uid;
}

function exact(meta: MissionMetadata, name: string, namespace: string | undefined, uid: string): boolean {
  return !!uid && !!meta.resourceVersion && !meta.deletionTimestamp
    && meta.name === name && meta.namespace === namespace && meta.uid === uid;
}
function observed(workload: Workload): boolean {
  return Number.isSafeInteger(workload.metadata.generation) && workload.metadata.generation! > 0
    && workload.status?.observedGeneration === workload.metadata.generation;
}
function withoutHash(template: PodTemplate): PodTemplate {
  const copy = structuredClone(template);
  delete copy.metadata?.labels?.[hashLabel];
  return copy;
}

/** Read-side counterpart of namespace_ownership and credential_source_workloads ownership rules. */
export function currentMissionWorkload(binding: MissionWorkloadBinding, workspace: string, resources: MissionWorkloadResources): boolean {
  const { namespace, sandbox, deployment, replicaSet, pod } = resources;
  const runtimeNamespace = `kars-${binding.sandboxName}`;
  const nsAnnotations = namespace.metadata.annotations ?? {};
  if (!exact(namespace.metadata, runtimeNamespace, undefined, binding.namespaceUid)
    || namespace.status?.phase !== "Active" || namespace.metadata.ownerReferences?.length
    || nsAnnotations[`${prefix}namespace-claim-version`] !== "v1"
    || nsAnnotations[`${prefix}sandbox-namespace`] !== workspace
    || nsAnnotations[`${prefix}sandbox-name`] !== binding.sandboxName
    || nsAnnotations[`${prefix}sandbox-uid`] !== binding.sandboxUid
    || nsAnnotations[`${prefix}namespace-prestage`] !== undefined
    || !exact(sandbox.metadata, binding.sandboxName, workspace, binding.sandboxUid)
    || sandbox.metadata.annotations?.[`${prefix}namespace-uid`] !== binding.namespaceUid
    || sandbox.spec?.suspended === true
    || sandbox.metadata.annotations?.[`${prefix}credential-rebind-task-uid`] !== undefined) return false;

  const labels = deployment.metadata.labels ?? {};
  const consumer = deployment.metadata.annotations?.[`${prefix}credential-sandbox-uid`] === binding.sandboxUid
    && deployment.metadata.annotations?.[`${prefix}credential-namespace-uid`] === binding.namespaceUid;
  const authored = deployment.metadata.managedFields?.some(entry => entry.manager === "kars-controller/karssandbox"
    && entry.operation === "Apply" && Object.hasOwn(entry.fieldsV1 ?? {}, "f:spec")) === true;
  if (!exact(deployment.metadata, binding.sandboxName, runtimeNamespace, binding.deploymentUid)
    || deployment.metadata.ownerReferences?.length || (!consumer && !authored)
    || labels[`${prefix}sandbox`] !== binding.sandboxName || labels[`${prefix}component`] !== "sandbox"
    || (labels[`${prefix}parent-namespace`] !== undefined && labels[`${prefix}parent-namespace`] !== workspace)
    || !Number.isSafeInteger(binding.deploymentGeneration) || binding.deploymentGeneration < 1
    || deployment.metadata.generation !== binding.deploymentGeneration || !observed(deployment)
    || deployment.spec?.paused === true || deployment.spec?.replicas !== 1
    || deployment.status?.replicas !== 1 || deployment.status.updatedReplicas !== 1
    || deployment.status.readyReplicas !== 1 || deployment.status.availableReplicas !== 1
    || (deployment.status.terminatingReplicas ?? 0) !== 0) return false;

  if (!exact(replicaSet.metadata, binding.replicaSetName, runtimeNamespace, binding.replicaSetUid)
    || !singleControllerOwner(replicaSet.metadata, "apps/v1", "Deployment", binding.sandboxName, binding.deploymentUid)
    || !observed(replicaSet) || replicaSet.spec?.replicas !== 1 || replicaSet.status?.replicas !== 1
    || replicaSet.status.readyReplicas !== 1 || replicaSet.status.availableReplicas !== 1
    || (replicaSet.status.terminatingReplicas ?? 0) !== 0) return false;
  const revision = deployment.metadata.annotations?.[revisionKey];
  const hash = replicaSet.metadata.labels?.[hashLabel];
  const desired = deployment.spec.template;
  const actual = replicaSet.spec.template;
  if (!revision || !/^[1-9][0-9]*$/.test(revision) || replicaSet.metadata.annotations?.[revisionKey] !== revision
    || !hash || actual?.metadata?.labels?.[hashLabel] !== hash || pod.metadata.labels?.[hashLabel] !== hash
    || !desired?.spec?.containers?.length || !actual?.spec?.containers?.length
    || !isDeepStrictEqual(withoutHash(desired), withoutHash(actual))) return false;

  return exact(pod.metadata, binding.podName, runtimeNamespace, binding.podUid)
    && singleControllerOwner(pod.metadata, "apps/v1", "ReplicaSet", binding.replicaSetName, binding.replicaSetUid)
    && pod.status?.phase === "Running" && pod.status.conditions?.some(c => c.type === "Ready" && c.status === "True") === true;
}
