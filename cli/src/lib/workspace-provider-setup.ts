// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { applyReviewedGrant, validateGrantDocument } from "../commands/credential-grants.js";
import { canonical, previewPrivateActivation, read, reviewed, validateQualifiedActivation, type Execute, type PrivateActivation } from "./private-activation.js";
import { PrivateBudgetRotationRequiredError, retirementState } from "./private-activation-retirement.js";

const resource = "karscredentialgrants.kars.azure.com";
const stores = [
  { name: "kars-inference-providers", purpose: "providers" },
  { name: "kars-credential-controller-settings", purpose: "controller-settings" },
  { name: "kars-github-app", purpose: "github-app" },
] as const;

type ObjectMap = Record<string, any>;
export interface ProviderSetupOptions {
  namespace: string;
  bridgeNamespace: string;
  controllerProfile: PrivateActivation["profile"];
  consumers?: string[];
}

async function optional(execute: Execute, kind: string, name: string, namespace: string): Promise<ObjectMap | undefined> {
  const text = await execute(["get", kind, name, "-n", namespace, "--ignore-not-found", "-o", "json"]);
  if (!text.trim() || text.trim() === "null") return undefined;
  const object = JSON.parse(text);
  reviewed(object);
  return object;
}

function compatibleGrant(grant: ObjectMap | undefined, namespaceUid: string): void {
  if (grant && (grant.spec?.workspaceUid !== namespaceUid || grant.spec.enabled !== true
    || !Array.isArray(grant.spec.writers) || !Array.isArray(grant.spec.integrationStores))) {
    throw new Error("Provider setup cannot adopt a replaced workspace, disabled grant, or malformed authority");
  }
}

export async function prepareWorkspaceProviderGrant(execute: Execute, options: ProviderSetupOptions): Promise<ObjectMap> {
  const ns = options.namespace;
  if ((await execute(["auth", "can-i", "manage", `${resource}/workspace`, "-n", ns])).trim() !== "yes") {
    throw new Error("Provider setup requires the installer's credential-grant operator permission");
  }
  const namespace = reviewed(await read(execute, "namespace", ns));
  const existing = await optional(execute, resource, "workspace", ns);
  compatibleGrant(existing, namespace.uid);
  const bff = await read(execute, "deployment", "kars-bridge-bff", options.bridgeNamespace) as ObjectMap;
  const accountName = bff.spec?.template?.spec?.serviceAccountName;
  if (typeof accountName !== "string" || !accountName || accountName === "default") {
    throw new Error("Provider setup requires Bridge's dedicated installed ServiceAccount");
  }
  const account = reviewed(await read(execute, "serviceaccount", accountName, options.bridgeNamespace));
  const controller = reviewed(await read(execute, "deployment", "kars-controller", ns));
  const writers = structuredClone(existing?.spec.writers ?? []) as { namespace: string; name: string; uid: string }[];
  const writer = writers.find(value => value.namespace === options.bridgeNamespace && value.name === accountName);
  if (writer && writer.uid !== account.uid) throw new Error("Enrolled Bridge writer identity changed; existing authority was preserved");
  if (!writer) writers.push({ namespace: options.bridgeNamespace, name: accountName, uid: account.uid });
  if (existing?.spec.controller && (existing.spec.controller.name !== controller.name || existing.spec.controller.uid !== controller.uid)) {
    throw new Error("Enrolled Core controller identity changed; existing authority was preserved");
  }
  const integrationStores = structuredClone(existing?.spec.integrationStores ?? []) as ObjectMap[];
  for (const selected of stores) {
    let store = await optional(execute, "secret", selected.name, ns);
    const enrolled = integrationStores.find(value => value.secret?.name === selected.name);
    if (enrolled && (!store || enrolled.secret.uid !== store.metadata.uid || enrolled.purpose !== selected.purpose)) {
      throw new Error("Enrolled provider store identity or purpose changed; existing credentials were preserved");
    }
    if (!store) {
      store = JSON.parse(await execute(["create", "-f", "-", "-o", "json"], JSON.stringify({
        apiVersion: "v1", kind: "Secret", type: "Opaque", metadata: { name: selected.name, namespace: ns },
      })));
    }
    const identity = reviewed(store);
    if (store!.type !== "Opaque") throw new Error("Provider setup requires an Opaque credential store; existing data was preserved");
    if (!enrolled) integrationStores.push({ secret: { name: selected.name, uid: identity.uid }, purpose: selected.purpose });
  }
  const consumers = [...new Set([
    ...(existing?.spec.privateActivation?.namespaces ?? []).flatMap((scope: ObjectMap) =>
      (scope.consumers ?? []).map((consumer: ObjectMap) => `${scope.namespace.name}/${consumer.kind}/${consumer.object.name}`)),
    `${options.bridgeNamespace}/Deployment/kars-bridge-bff`, ...(options.consumers ?? []),
  ])] as string[];
  const privateActivation = await previewPrivateActivation(execute, ns, writers, existing?.spec.observationTargets ?? [],
    ns, options.controllerProfile, consumers);
  const document = {
    apiVersion: "kars.azure.com/v1alpha1", kind: "KarsCredentialGrant",
    metadata: { name: "workspace", namespace: ns, ...(existing ? { uid: existing.metadata.uid, resourceVersion: existing.metadata.resourceVersion } : {}) },
    spec: { agentKeys: [], legacyImports: [], observationTargets: [], githubConnections: [], ...existing?.spec,
      workspaceUid: namespace.uid, enabled: true, writers, integrationStores, privateActivation,
      controller: { name: controller.name, uid: controller.uid } },
  };
  await validateGrantDocument(execute, document);
  return document;
}

export async function setupWorkspaceProviders(execute: Execute, options: ProviderSetupOptions,
  rotateBudget?: (activation: PrivateActivation) => Promise<void>): Promise<void> {
  let document = await prepareWorkspaceProviderGrant(execute, options);
  const current = await optional(execute, resource, "workspace", options.namespace);
  if (current?.metadata.uid === document.metadata.uid && current?.spec.privateActivation?.phase === "qualified") {
    const { privateActivation: previous, ...authority } = current.spec;
    const { privateActivation: next, ...requested } = document.spec;
    const covered = next.namespaces.every((scope: ObjectMap) => previous.namespaces.some((old: ObjectMap) =>
      old.namespace.uid === scope.namespace.uid && scope.consumers.every((consumer: ObjectMap) =>
        old.consumers.some((known: ObjectMap) => known.kind === consumer.kind && known.object.uid === consumer.object.uid))));
    if (covered && canonical(authority) === canonical(requested)) {
      await validateQualifiedActivation(execute, previous);
      return;
    }
  }
  const namespace = await read(execute, "namespace", options.namespace);
  const retirement = retirementState(namespace);
  if (rotateBudget && retirement?.phase === "retired") {
    await rotateBudget(document.spec.privateActivation);
    document = await prepareWorkspaceProviderGrant(execute, options);
  }
  try {
    await applyReviewedGrant(execute, document);
  } catch (error) {
    if (!(error instanceof PrivateBudgetRotationRequiredError) || !rotateBudget) throw error;
    // Retirement changes namespace versions. Never reuse the pre-retirement review.
    document = await prepareWorkspaceProviderGrant(execute, options);
    await rotateBudget(document.spec.privateActivation);
    document = await prepareWorkspaceProviderGrant(execute, options);
    await applyReviewedGrant(execute, document);
  }
}

export async function waitForWorkspaceProviders(execute: Execute, namespace: string): Promise<void> {
  const initial = await optional(execute, resource, "workspace", namespace);
  if (!initial) throw new Error("Provider setup did not publish its workspace grant");
  await execute(["wait", "-n", namespace, `${resource}/workspace`,
    `--for=jsonpath={.status.observedGeneration}=${initial.metadata.generation}`, "--timeout=900s"]);
  for (const condition of ["Ready", "WriterReady", "IntegrationReady", "PrivateConsumptionReady"]) {
    await execute(["wait", "-n", namespace, `${resource}/workspace`, `--for=condition=${condition}`, "--timeout=900s"]);
  }
  const current = await optional(execute, resource, "workspace", namespace);
  if (!current || current.metadata.uid !== initial.metadata.uid || current.metadata.generation !== initial.metadata.generation
    || current.status?.observedGeneration !== current.metadata.generation
    || ["Ready", "WriterReady", "IntegrationReady", "PrivateConsumptionReady"].some(type =>
      !current.status.conditions?.some((condition: ObjectMap) => condition.type === type && condition.status === "True"))) {
    throw new Error("Provider enrollment changed during readiness verification; installation is not complete");
  }
}
