// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash } from "node:crypto";
import { at, canonical, digest, read, record, reviewed, templateDigest, type Execute, type PrivateActivation } from "./private-activation.js";

type ObjectValue = ReturnType<typeof record>;
const ERROR = "Root migration SRE reconciliation does not match its enrolled source and current credentials";
const OWNER = "kars.azure.com/sre-registration-uid";
const PRIVACY = "kars.azure.com/sre-privacy-epoch";
export const SRE_PROJECTION_FIELDS = [
  "kars.azure.com/inference-budget-ca-version",
  "kars.azure.com/inference-providers-version",
  "kars.azure.com/sre-private-credential-uid",
  "kars.azure.com/sre-tls-expiry",
] as const;

export interface RootSreProjection {
  sourceUid: string;
  sourceGeneration: number;
  replicas: 0 | 1;
  runtime?: {
    controller: ObjectValue;
    kind: "OpenClaw" | "Hermes";
    openclawImage?: string;
    routerImage?: string;
  };
}

function imageSlot(value: ObjectValue, list: string, name: string): ObjectValue {
  const containers = at(value, "spec", "template", "spec", list);
  const matches = Array.isArray(containers) ? containers.filter(value => at(value, "name") === name) : [];
  if (matches.length !== 1 || typeof at(matches[0], "image") !== "string") throw new Error(ERROR);
  return record(matches[0]);
}

const runtimeSlots = [["containers", "agent"], ["initContainers", "egress-guard"]] as const;

export function restoreSreRuntimeImages(value: ObjectValue, source: ObjectValue, router = false): void {
  for (const [list, name] of runtimeSlots) imageSlot(value, list, name).image = imageSlot(source, list, name).image;
  if (router) imageSlot(value, "containers", "inference-router").image = imageSlot(source, "containers", "inference-router").image;
}

function routerChanged(before: ObjectValue, after: ObjectValue): boolean {
  const images = (value: ObjectValue) => {
    const entries = at(value, "spec", "template", "spec", "containers");
    return Array.isArray(entries) ? entries.filter(value => at(value, "name") === "inference-router").map(value => at(value, "image")) : [];
  };
  return canonical(images(before)) !== canonical(images(after));
}

function runtimeChanged(before: ObjectValue, after: ObjectValue): boolean {
  return routerChanged(before, after) || runtimeSlots.some(([list, name]) => {
    const images = (value: ObjectValue) => {
      const entries = at(value, "spec", "template", "spec", list);
      return Array.isArray(entries) ? entries.filter(value => at(value, "name") === name).map(value => at(value, "image")) : [];
    };
    return canonical(images(before)) !== canonical(images(after));
  });
}

function validateRuntime(projection: RootSreProjection, after: ObjectValue, activation: PrivateActivation): void {
  const runtime = projection.runtime;
  if (!runtime) return;
  const controller = runtime.controller;
  if (at(controller, "metadata", "uid") !== activation.root.deployment.uid
    || at(controller, "metadata", "name") !== activation.root.deployment.name
    || at(controller, "metadata", "namespace") !== activation.root.namespace.name
    || templateDigest(controller) !== activation.root.templateDigest
    || !["OpenClaw", "Hermes"].includes(runtime.kind)
    || (runtime.openclawImage !== undefined && (runtime.kind !== "OpenClaw" || typeof runtime.openclawImage !== "string"))) throw new Error(ERROR);
  const containers = at(controller, "spec", "template", "spec", "containers");
  const roots = Array.isArray(containers) ? containers.filter(value => at(value, "name") === "controller") : [];
  if (roots.length !== 1) throw new Error(ERROR);
  const env = at(roots[0], "env");
  const image = (key: string): string => {
    const values = Array.isArray(env) ? env.filter(value => at(value, "name") === key) : [];
    const value = values.length === 1 ? at(values[0], "value") : undefined;
    if (typeof value !== "string" || !/^.+:latest@sha256:[a-f0-9]{64}$/.test(value)) throw new Error(ERROR);
    return value;
  };
  const guard = image("SANDBOX_IMAGE");
  const agent = runtime.kind === "Hermes" ? image("HERMES_RUNTIME_IMAGE") : runtime.openclawImage ?? guard;
  if (!/^.+:latest@sha256:[a-f0-9]{64}$/.test(agent)
    || imageSlot(after, "containers", "agent").image !== agent
    || imageSlot(after, "initContainers", "egress-guard").image !== guard) throw new Error(ERROR);
  if (runtime.routerImage !== undefined && (runtime.routerImage !== image("INFERENCE_ROUTER_IMAGE")
    || imageSlot(after, "containers", "inference-router").image !== runtime.routerImage)) throw new Error(ERROR);
}

export function validateSreProjection(projection: RootSreProjection, before: ObjectValue, after: ObjectValue, activation: PrivateActivation): void {
  if (!projection || typeof projection.sourceUid !== "string" || !projection.sourceUid
    || !Number.isSafeInteger(projection.sourceGeneration) || projection.sourceGeneration < 1
    || ![0, 1].includes(projection.replicas) || at(after, "spec", "replicas") !== projection.replicas
    || SRE_PROJECTION_FIELDS.some(key => [before, after].some(value =>
      typeof at(value, "spec", "template", "metadata", "annotations", key) !== "string"))) throw new Error(ERROR);
  if (runtimeChanged(before, after) !== !!projection.runtime
    || routerChanged(before, after) !== (projection.runtime?.routerImage !== undefined)) throw new Error(ERROR);
  validateRuntime(projection, after, activation);
}

/** Reconcile source-derived annotations, run/hold intent and reviewed controller runtime defaults. */
export async function reviewSreProjection(run: Execute, activation: PrivateActivation, namespace: string,
  deployment: ObjectValue, before: ObjectValue): Promise<RootSreProjection> {
  const scope = activation.namespaces.find(value => value.namespace.name === namespace);
  const reg = await read(run, "karssreregistrations.kars.azure.com", "canonical");
  const identity = reviewed(reg), spec = record(reg.spec), status = record(reg.status);
  const source = record(spec.sandbox), runtime = record(spec.runtimeNamespace);
  if (!scope || namespace !== "kars-sre" || runtime.name !== namespace || runtime.uid !== scope.namespace.uid
    || source.name !== reviewed(deployment).name || source.namespace !== activation.root.namespace.name
    || spec.enabled !== true || status.phase !== "Ready" || !Number.isSafeInteger(at(reg, "metadata", "generation"))
    || Number(at(reg, "metadata", "generation")) < 1 || status.observedGeneration !== at(reg, "metadata", "generation")
    || status.legacySecretAccessDenied !== true || typeof status.privacyEpoch !== "string" || !status.privacyEpoch) throw new Error(ERROR);
  const sandbox = await read(run, "karssandbox", String(source.name), String(source.namespace));
  const uid = reviewed(sandbox).uid, generation = at(sandbox, "metadata", "generation");
  if (uid !== source.uid || !Number.isSafeInteger(generation) || Number(generation) < 1
    || at(sandbox, "metadata", "annotations", "kars.azure.com/namespace-uid") !== scope.namespace.uid) throw new Error(ERROR);
  const suspended = at(sandbox, "spec", "suspended");
  if (suspended !== undefined && suspended !== null && typeof suspended !== "boolean") throw new Error(ERROR);
  const held = at(sandbox, "metadata", "annotations", "kars.azure.com/credential-rebind-task-uid") !== undefined;
  const replicas = suspended === true || held ? 0 : 1;
  if (at(deployment, "spec", "replicas") !== replicas) throw new Error(ERROR);
  const tls = await read(run, "secret", "sre-api-router-identity", namespace);
  const tlsUid = reviewed(tls).uid;
  if (at(tls, "metadata", "annotations", OWNER) !== identity.uid
    || at(tls, "metadata", "annotations", PRIVACY) !== status.privacyEpoch) throw new Error(ERROR);
  const expires = at(tls, "metadata", "annotations", "kars.azure.com/sre-tls-expiry");
  if (typeof expires !== "string" || !/^\d+$/.test(expires) || Number(expires) <= Date.now() / 1000) throw new Error(ERROR);
  const providers = await read(run, "secret", "kars-inference-providers", String(source.namespace));
  const providerUid = reviewed(providers).uid;
  const data = providers.data == null ? null : record(providers.data);
  if (data && Object.values(data).some(value => typeof value !== "string")) throw new Error(ERROR);
  const catalog = await read(run, "configmap", "kars-inference-budget-contracts", activation.root.namespace.name);
  reviewed(catalog);
  const ca = at(catalog, "data", "ca.crt");
  if (!activation.root.budgetTls || at(catalog, "metadata", "annotations", "kars.azure.com/inference-budget-contracts") !== "v1"
    || typeof ca !== "string" || !ca.includes("-----BEGIN CERTIFICATE-----")
    || ca.includes("PRIVATE KEY") || ca.length > 65_536) throw new Error(ERROR);
  const expected = {
    [SRE_PROJECTION_FIELDS[0]]: createHash("sha256").update(ca).digest("hex"),
    [SRE_PROJECTION_FIELDS[1]]: digest([providerUid, data]),
    [SRE_PROJECTION_FIELDS[2]]: tlsUid,
    [SRE_PROJECTION_FIELDS[3]]: expires,
    [OWNER]: identity.uid,
    [PRIVACY]: status.privacyEpoch,
  };
  const fields = record(at(deployment, "spec", "template", "metadata", "annotations"));
  if (Object.entries(expected).some(([key, value]) => fields[key] !== value)) throw new Error(ERROR);
  const projection: RootSreProjection = { sourceUid: uid, sourceGeneration: Number(generation), replicas };
  if (runtimeChanged(before, deployment)) {
    const kind = at(sandbox, "spec", "runtime", "kind") ?? "OpenClaw";
    if (kind !== "OpenClaw" && kind !== "Hermes") throw new Error(ERROR);
    const controller = await read(run, "deployment", activation.root.deployment.name, activation.root.namespace.name);
    const openclawImage = at(sandbox, "spec", "runtime", "openclaw", "image");
    if (openclawImage != null && typeof openclawImage !== "string") throw new Error(ERROR);
    projection.runtime = {
      controller: { metadata: { uid: reviewed(controller).uid, name: reviewed(controller).name,
        namespace: activation.root.namespace.name }, spec: { template: record(at(controller, "spec", "template")) } },
      kind, ...(kind === "OpenClaw" && openclawImage != null ? { openclawImage } : {}),
      ...(routerChanged(before, deployment) ? { routerImage: String(imageSlot(deployment, "containers", "inference-router").image) } : {}),
    };
    validateRuntime(projection, deployment, activation);
  }
  return projection;
}
