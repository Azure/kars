// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { timingSafeEqual } from "node:crypto";
import { isDeepStrictEqual } from "node:util";
import type { KubernetesJson } from "./kubernetes-json.js";
import { singleControllerOwner, type MissionMetadata, type MissionWorkloadResources } from "./mission-workload.js";

export const dispatcherRootName = "kars-mission-dispatcher-identity";
const prefix = "kars.azure.com/";
const pin = `${prefix}mission-root-uid`;
const hash = "pod-template-hash";
const revision = "deployment.kubernetes.io/revision";
export interface DispatcherLocation { namespace: string; deployment: string; release: string; podName: string; podUid: string }
export interface DispatcherCustody { deploymentUid: string; podUid: string; namespaceUid: string; rootUid: string }
type Workload = MissionWorkloadResources["deployment"] & { spec?: { strategy?: { type?: string } } };
type Pod = MissionWorkloadResources["pod"] & { spec?: NonNullable<NonNullable<Workload["spec"]>["template"]>["spec"] };
interface Secret { metadata: MissionMetadata; type?: string; immutable?: boolean; data?: Record<string, string> }
interface List<T> { items: T[]; metadata?: { continue?: string } }
const name = (value: string): boolean => /^[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?$/.test(value) && value.length <= 253;
const label = (value: string): boolean => name(value) && value.length <= 63 && !value.includes(".");
function requireCondition(value: unknown): asserts value { if (!value) throw new Error("Dispatcher workload custody is not current"); }
function identified(meta: MissionMetadata, expectedName: string, namespace?: string): void {
  requireCondition(meta?.name === expectedName && meta.namespace === namespace && meta.uid && meta.resourceVersion && !meta.deletionTimestamp);
}
function observed(w: Workload): boolean {
  return Number.isSafeInteger(w.metadata.generation) && w.metadata.generation! > 0 && w.status?.observedGeneration === w.metadata.generation;
}
function rootSource(spec: { containers?: Array<{ name: string; [key: string]: unknown }> } | undefined): void {
  const containers = spec?.containers;
  requireCondition(containers?.length === 1 && containers[0].name === "mission-dispatcher");
  const env = containers[0].env;
  requireCondition(Array.isArray(env));
  const entries = env.filter(e => e?.name === "KARS_MISSION_IDENTITY_ROOT");
  requireCondition(entries.length === 1 && isDeepStrictEqual(entries[0], {
    name: "KARS_MISSION_IDENTITY_ROOT", valueFrom: { secretKeyRef: { name: dispatcherRootName, key: "root", optional: false } },
  }));
}
function withoutHash(template: NonNullable<NonNullable<Workload["spec"]>["template"]>) {
  const copy = structuredClone(template); delete copy.metadata?.labels?.[hash]; return copy;
}

/** Does not require this Pod's Ready condition: connecting the SDK precedes readiness. */
export async function dispatcherCustody(api: KubernetesJson, location: DispatcherLocation, mountedRoot: string): Promise<DispatcherCustody> {
  const { namespace, deployment: deploymentName, release, podName, podUid } = location;
  requireCondition([namespace, deploymentName, release].every(label) && name(podName) && podUid);
  const nsPath = `/api/v1/namespaces/${namespace}`;
  const deploymentPath = `/apis/apps/v1/namespaces/${namespace}/deployments/${deploymentName}`;
  const podPath = `${nsPath}/pods/${podName}`;
  const ns = await api.request<MissionWorkloadResources["namespace"]>("GET", nsPath);
  identified(ns.metadata, namespace); requireCondition(ns.status?.phase === "Active" && !ns.metadata.ownerReferences?.length);
  const deployment = await api.request<Workload>("GET", deploymentPath);
  const dm = deployment.metadata;
  identified(dm, deploymentName, namespace);
  requireCondition(!dm.ownerReferences?.length && dm.labels?.["app.kubernetes.io/managed-by"] === "Helm"
    && dm.labels?.["app.kubernetes.io/component"] === "mission-dispatcher"
    && dm.annotations?.["meta.helm.sh/release-name"] === release && dm.annotations?.["meta.helm.sh/release-namespace"] === namespace
    && dm.annotations?.[pin] && deployment.spec?.replicas === 1 && deployment.spec.paused !== true
    && deployment.spec.strategy?.type === "Recreate" && observed(deployment)
    && deployment.status?.replicas === 1 && deployment.status.updatedReplicas === 1 && !deployment.status.terminatingReplicas);
  rootSource(deployment.spec.template?.spec);
  const pod = await api.request<Pod>("GET", podPath);
  identified(pod.metadata, podName, namespace);
  requireCondition(pod.metadata.uid === podUid && pod.status?.phase === "Running");
  rootSource(pod.spec);
  const parents = pod.metadata.ownerReferences?.filter(o => o.controller === true) ?? [];
  requireCondition(parents.length === 1 && parents[0].apiVersion === "apps/v1" && parents[0].kind === "ReplicaSet" && name(parents[0].name));
  const rsPath = `/apis/apps/v1/namespaces/${namespace}/replicasets/${parents[0].name}`;
  const rs = await api.request<Workload>("GET", rsPath);
  identified(rs.metadata, parents[0].name, namespace);
  requireCondition(rs.metadata.uid === parents[0].uid && singleControllerOwner(rs.metadata, "apps/v1", "Deployment", deploymentName, dm.uid!)
    && observed(rs) && rs.spec?.replicas === 1 && rs.status?.replicas === 1 && !rs.status.terminatingReplicas
    && /^[1-9][0-9]*$/.test(dm.annotations?.[revision] ?? "") && rs.metadata.annotations?.[revision] === dm.annotations?.[revision]
    && pod.metadata.labels?.[hash] && pod.metadata.labels[hash] === rs.metadata.labels?.[hash]
    && rs.metadata.labels?.[hash] === rs.spec?.template?.metadata?.labels?.[hash]
    && deployment.spec.template && rs.spec?.template
    && isDeepStrictEqual(withoutHash(deployment.spec.template), withoutHash(rs.spec.template)));
  const setsPath = `/apis/apps/v1/namespaces/${namespace}/replicasets?limit=1000`;
  const podsPath = `${nsPath}/pods?limit=1000`;
  const sets = await api.request<List<Workload>>("GET", setsPath);
  const pods = await api.request<List<Pod>>("GET", podsPath);
  requireCondition(Array.isArray(sets.items) && Array.isArray(pods.items) && !sets.metadata?.continue && !pods.metadata?.continue);
  const ownedSets = sets.items.filter(s => s.metadata.ownerReferences?.some(o => o.uid === dm.uid));
  requireCondition(ownedSets.some(s => s.metadata.uid === rs.metadata.uid));
  for (const set of ownedSets) {
    requireCondition(singleControllerOwner(set.metadata, "apps/v1", "Deployment", deploymentName, dm.uid!));
    if (set.metadata.uid !== rs.metadata.uid) requireCondition(set.spec?.replicas === 0
      && [set.status?.replicas, set.status?.readyReplicas, set.status?.availableReplicas, set.status?.terminatingReplicas].every(n => !n));
  }
  const setIds = new Set(ownedSets.map(s => s.metadata.uid));
  const ownedPods = pods.items.filter(p => p.metadata.ownerReferences?.some(o => setIds.has(o.uid)));
  requireCondition(ownedPods.length === 1 && ownedPods[0].metadata.uid === podUid);

  // A root CREATE can start the container before its UID pin commits. Never GET/use it before that pin.
  const secretPath = `${nsPath}/secrets/${dispatcherRootName}`;
  const secret = await api.request<Secret>("GET", secretPath);
  const sm = secret.metadata;
  identified(sm, dispatcherRootName, namespace);
  requireCondition(sm.uid === dm.annotations?.[pin] && sm.ownerReferences?.length === 1
    && singleControllerOwner(sm, "apps/v1", "Deployment", deploymentName, dm.uid!)
    && (sm.ownerReferences[0] as { blockOwnerDeletion?: boolean }).blockOwnerDeletion === true
    && sm.annotations?.[`${prefix}mission-root-version`] === "v1"
    && sm.annotations?.[`${prefix}mission-owner-uid`] === dm.uid
    && sm.annotations?.[`${prefix}mission-namespace-uid`] === ns.metadata.uid
    && sm.annotations?.[`${prefix}mission-identity-role`] === "dispatcher"
    && secret.immutable === true && secret.type === "Opaque" && Object.keys(secret.data ?? {}).length === 1
    && typeof secret.data?.root === "string" && /^[a-fA-F0-9]{64}$/.test(mountedRoot));
  const bytes = Buffer.from(secret.data.root, "base64");
  requireCondition(bytes.toString("base64") === secret.data.root && /^[a-fA-F0-9]{64}$/.test(bytes.toString("utf8"))
    && bytes.length === 64 && timingSafeEqual(bytes, Buffer.from(mountedRoot)));
  for (const [path, prior] of [[secretPath, secret], [rsPath, rs], [podPath, pod], [deploymentPath, deployment], [nsPath, ns]] as const) {
    const fresh = await api.request<{ metadata: MissionMetadata }>("GET", path);
    identified(fresh.metadata, prior.metadata.name, prior.metadata.namespace);
    requireCondition(fresh.metadata.uid === prior.metadata.uid && fresh.metadata.resourceVersion === prior.metadata.resourceVersion);
  }
  return { deploymentUid: dm.uid!, podUid, namespaceUid: ns.metadata.uid!, rootUid: sm.uid! };
}
