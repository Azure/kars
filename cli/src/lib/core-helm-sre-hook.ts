// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash } from "node:crypto";
import { canonicalSchema, schemaDocuments, type SchemaExecute } from "./schema-documents.js";
import { assertRenderedControllersMutable } from "./private-root-upgrade-guard.js";
import { stageSource } from "./sre-source.js";
import { enroll, get, preview, registration, waitForAuthority, type Execute, type Enrollment } from "./sre-authority.js";
import { parseFirstInstallFunding } from "./sre-funding.js";
import { authorizeUnbounded } from "../commands/budget-unbounded.js";
import { bridgeReactivationDigest, fundingView } from "../commands/budget-topup.js";
import type { CoreSchemaHookOptions } from "./core-helm-schema-hook.js";

const INTENT = "kars-sre-first-install";
const matches = (actual: any, desired: any): boolean => Array.isArray(desired)
  ? Array.isArray(actual) && actual.length === desired.length && desired.every((item, i) => matches(actual[i], item))
  : desired !== null && typeof desired === "object"
    ? actual !== null && typeof actual === "object" && Object.keys(desired).every(key => matches(actual[key], desired[key]))
    : actual === desired;

function controllerEnvironmentDefaults(spec: Record<string, any>): Record<string, any> {
  const normalized = structuredClone(spec);
  for (const field of ["containers", "initContainers"]) {
    for (const container of normalized.template?.spec?.[field] ?? []) {
      for (const variable of container.env ?? []) {
        // The API omits EnvVar.value when empty; valueFrom is not an empty literal.
        if (variable.value === undefined && variable.valueFrom === undefined) variable.value = "";
      }
    }
  }
  return normalized;
}

/** A single original-install attempt. No recovery/adoption, Helm mutation or spending. */
export async function completePendingCoreSre(execute: SchemaExecute, options: CoreSchemaHookOptions) {
  if (options.operation !== "install" || options.revision !== 1 || options.release !== "kars"
    || options.namespace !== "kars-system" || !/^v4\.1\.3(?:\+[0-9A-Za-z.-]+)?$/.test(options.installerHelmVersion)) {
    throw new Error("SRE bootstrap requires the original kars/kars-system revision-one Helm 4.1.3 install");
  }
  const timeout = options.timeoutMs ?? 600_000;
  if (!Number.isSafeInteger(timeout) || timeout < 1 || timeout > 600_000) throw new Error("Invalid SRE bootstrap deadline");
  const now = options.now ?? (() => performance.now());
  const deadline = now() + timeout;
  const invoke: SchemaExecute = async (file, args, settings) => {
    const remaining = Math.floor(deadline - now());
    if (remaining <= 0) throw new Error("SRE bootstrap deadline expired");
    const result = await execute(file, args, { ...settings, timeout: Math.min(25_000, remaining) });
    if (now() >= deadline || Buffer.byteLength(result.stdout) > 8 * 1024 * 1024) throw new Error("SRE bootstrap response/deadline bound exceeded");
    return result;
  };
  const read = invoke as Execute;
  const history = async () => {
    const result = JSON.parse((await invoke("helm", ["history", options.release, "-n", options.namespace,
      "-o", "json", "--max", "2"], { stdio: "pipe" })).stdout);
    if (!Array.isArray(result) || result.length !== 1 || Number(result[0].revision) !== 1 || result[0].status !== "pending-install") {
      throw new Error("SRE bootstrap is not the sole original pending install");
    }
  };
  const manifest = async () => (await invoke("helm", ["get", "manifest", options.release, "-n", options.namespace,
    "--revision", "1"], { stdio: "pipe" })).stdout;
  await history();
  const pinned = await manifest();
  const documents = schemaDocuments(pinned);
  const desired = documents.filter(object => object.kind === "ConfigMap" && object.metadata.name === INTENT);
  if (desired.length !== 1 || desired[0].immutable !== true || desired[0].metadata.namespace !== options.namespace) {
    throw new Error("Missing exact immutable SRE intent in pending Core manifest");
  }
  const encoded = desired[0].data?.["intent.json"];
  if (typeof encoded !== "string" || Buffer.byteLength(encoded) > 131072) throw new Error("Invalid SRE intent bound");
  const intent = JSON.parse(encoded);
  const resources = intent.resources as Array<Record<string, any>>;
  if (intent.version !== 1 || intent.release !== options.release || intent.namespace !== options.namespace
    || !/^[a-f0-9]{64}$/.test(intent.valuesDigest) || !Array.isArray(resources) || resources.length !== 3
    || !intent.budget?.enabled || !Array.isArray(intent.budget.contracts) || !intent.budget.contracts.length
    || !intent.funding) throw new Error("Incomplete operator SRE intent");
  const funding = parseFirstInstallFunding(intent.funding);
  for (const [kind, name] of [["KarsSandbox", "sre"], ["InferencePolicy", "sre-inference"], ["ToolPolicy", "sre-tools"]]) {
    const matches = resources.filter(object => object.kind === kind && object.metadata?.name === name
      && object.metadata.namespace === options.namespace && object.apiVersion === "kars.azure.com/v1alpha1");
    if (matches.length !== 1 || await get(read, kind.toLowerCase(), name, options.namespace)) {
      throw new Error("SRE bootstrap requires three exact, absent custom resources; no adoption");
    }
  }
  if (resources.find(object => object.kind === "KarsSandbox")?.metadata.annotations?.["kars.azure.com/sre-first-install-intent"] !== INTENT) {
    throw new Error("SRE source omitted its immutable funding provenance");
  }
  if (await registration(read) || await get(read, "namespace", "kars-sre")) throw new Error("SRE bootstrap cannot resume an existing occupant or enrollment");
  await assertRenderedControllersMutable(invoke, documents, options.namespace);
  const installedIntent = await get(read, "configmap", INTENT, options.namespace);
  const catalog = await get(read, "configmap", "kars-inference-budget-contracts", options.namespace);
  const expectedCatalog = documents.find(object => object.kind === "ConfigMap" && object.metadata.name === "kars-inference-budget-contracts");
  const controller = await get(read, "deployment", "kars-controller", options.namespace);
  const controllerDesired = documents.find(object => object.kind === "Deployment" && object.metadata.name === "kars-controller");
  const workspace = await get(read, "namespace", options.namespace);
  const cluster = await get(read, "namespace", "kube-system");
  if (!installedIntent || !catalog || !expectedCatalog || !controller || !controllerDesired || !workspace || !cluster) throw new Error("Core prerequisites are absent");
  const owned = (object: NonNullable<typeof installedIntent>) => object.metadata.annotations?.["meta.helm.sh/release-name"] === options.release
    && object.metadata.annotations?.["meta.helm.sh/release-namespace"] === options.namespace;
  const check = async () => {
    await history();
    if (await manifest() !== pinned) throw new Error("Pinned Core manifest changed");
    const liveIntent = await get(read, "configmap", INTENT, options.namespace);
    const liveCatalog = await get(read, "configmap", "kars-inference-budget-contracts", options.namespace);
    const liveController = await get(read, "deployment", "kars-controller", options.namespace);
    if (!liveCatalog || liveCatalog.metadata.uid !== catalog.metadata.uid || !owned(liveCatalog)
      || liveCatalog.metadata.deletionTimestamp || canonicalSchema(liveCatalog.data) !== canonicalSchema(expectedCatalog.data)) {
      throw new Error("Operator catalog changed during SRE enrollment");
    }
    if (!liveIntent || !liveController || !owned(liveIntent) || !owned(liveController)
      || liveIntent.metadata.uid !== installedIntent.metadata.uid || liveIntent.metadata.deletionTimestamp
      || liveIntent.immutable !== true || canonicalSchema(liveIntent.data) !== canonicalSchema(desired[0].data)
      || liveController.metadata.uid !== controller.metadata.uid || liveController.metadata.deletionTimestamp
      || (liveController.status?.observedGeneration ?? 0) !== liveController.metadata.generation
      || (liveController.status?.availableReplicas ?? 0) < 1) throw new Error("Core identity or immutable intent is not ready");
    if (!matches(controllerEnvironmentDefaults(liveController.spec!), controllerEnvironmentDefaults(controllerDesired.spec))
      || canonicalSchema(liveController.spec) !== canonicalSchema(controller.spec)) {
      throw new Error("Core controller intent changed");
    }
    for (const original of [workspace, cluster]) {
      const live = await get(read, "namespace", original.metadata.name!);
      if (!live || live.metadata.uid !== original.metadata.uid || live.metadata.deletionTimestamp) throw new Error("SRE workspace/cluster identity changed");
    }
    for (const resource of resources) {
      const kind = resource.kind.toLowerCase();
      if (!createdObjects.has(kind)) continue;
      const live = await get(read, kind, resource.metadata.name, options.namespace);
      const actual = { ...live?.spec };
      if (kind === "karssandbox") delete actual.inferenceBudgetRef;
      if (!live || live.metadata.uid !== createdObjects.get(kind) || live.metadata.deletionTimestamp
        || !owned(live) || canonicalSchema(actual) !== canonicalSchema(createdSpecs.get(kind))
        || (kind === "karssandbox" && live.metadata.annotations?.["kars.azure.com/sre-first-install-intent"] !== INTENT)) {
        throw new Error("Created SRE source/support identity or configuration changed");
      }
    }
    if (expectedRegistration) {
      const runtime = await get(read, "namespace", "kars-sre");
      if (!runtime || runtime.metadata.deletionTimestamp || runtime.metadata.uid !== expectedRegistration.runtimeNamespace.uid) {
        throw new Error("Created SRE runtime identity changed");
      }
    }
    parseFirstInstallFunding(funding);
  };
  let expectedRegistration: Enrollment | undefined;
  let createdRegistrationUid: string | undefined;
  const createdObjects = new Map<string, string>();
  const createdSpecs = new Map<string, unknown>();
  const guarded: Execute = async (file, args, settings) => {
    if (file !== "kubectl" || !["get", "auth", "create"].includes(args[0])) throw new Error("SRE hook refused an unreviewed operation");
    if (args[0] === "create") {
      const object = JSON.parse(settings.input ?? "null");
      const resource = resources.find(item => item.kind === object?.kind && item.metadata.name === object?.metadata?.name);
      const expected = object?.kind === "KarsSRERegistration" ? expectedRegistration : resource?.spec;
      const metadata = resource && { ...resource.metadata,
        labels: { ...resource.metadata.labels, "app.kubernetes.io/managed-by": "Helm" },
        annotations: { ...resource.metadata.annotations, "meta.helm.sh/release-name": options.release,
          "meta.helm.sh/release-namespace": options.namespace } };
      if (object?.apiVersion !== "kars.azure.com/v1alpha1" || !expected || canonicalSchema(expected) !== canonicalSchema(object.spec)
        || (resource && canonicalSchema(metadata) !== canonicalSchema(object.metadata))
        || (object.kind === "KarsSRERegistration" ? object.metadata.name !== "canonical" || object.metadata.namespace !== undefined
          : object.metadata.namespace !== options.namespace)) throw new Error("SRE hook CREATE escaped its pinned intent");
      await check();
    }
    const result = await invoke(file, args[0] === "create" && !args.includes("-o") ? [...args, "-o", "json"] : args, settings);
    if (args[0] === "create") {
      const request = JSON.parse(settings.input!);
      const created = JSON.parse(result.stdout);
      if (created.kind !== request.kind || created.apiVersion !== request.apiVersion
        || !created.metadata?.uid || !created.metadata.resourceVersion || created.metadata.deletionTimestamp
        || !matches(created.metadata, request.metadata) || !matches(created.spec, request.spec)) {
        throw new Error("SRE CREATE omitted its exact committed identity; no adoption");
      }
      createdObjects.set(created.kind.toLowerCase(), created.metadata.uid);
      const specification = { ...created.spec };
      if (created.kind === "KarsSandbox") delete specification.inferenceBudgetRef;
      createdSpecs.set(created.kind.toLowerCase(), specification);
      if (created.kind === "KarsSRERegistration") createdRegistrationUid = created.metadata.uid;
    }
    if (args[0] === "get" && ["karssandbox", "inferencepolicy", "toolpolicy"].includes(args[1]) && result.stdout.trim()) {
      const live = JSON.parse(result.stdout);
      if (!createdObjects.has(args[1]) || live.metadata?.uid !== createdObjects.get(args[1])) {
        throw new Error("SRE source/support occupant raced this attempt; no adoption");
      }
    }
    return result;
  };
  await check();
  const created = await stageSource(guarded, resources.map(object => JSON.stringify(object)).join("\n---\n"), options.namespace, options.release);
  const spec = await preview(guarded, options.namespace, options.release);
  const source = await get(read, "karssandbox", "sre", options.namespace);
  const runtime = await get(read, "namespace", "kars-sre");
  if (spec.legacyBindings.length || spec.legacyConsumer || spec.sandbox.uid !== created.uid
    || spec.runtimeNamespace.uid !== created.namespaceUid || source?.spec?.inferenceBudgetRef
    || runtime?.metadata.annotations?.["kars.azure.com/calendar-initialization-uid"] !== created.uid
    || runtime?.metadata.annotations?.["kars.azure.com/calendar-account-uid"]) throw new Error("Fresh SRE provenance changed before enrollment");
  spec.firstInstallFunding = {
    intent: { name: INTENT, uid: installedIntent.metadata.uid!, digest: createHash("sha256").update(encoded).digest("hex") },
    grant: { root: { kind: "KarsSandbox", resource: { namespace: options.namespace, name: "sre", uid: created.uid },
      workspaceUid: workspace.metadata.uid!, clusterUid: cluster.metadata.uid! },
      ...("unboundedTokens" in funding ? { limits: {}, unboundedTokens: true as const }
        : { limits: { tokens: funding.tokens }, expiresAt: funding.expiresAt }) },
  };
  expectedRegistration = spec;
  await enroll(guarded, spec, { sandboxUid: created.uid, namespaceUid: created.namespaceUid, binding: [] }, false);
  await waitForAuthority(guarded, "Ready");
  let allowance: Record<string, any> | undefined;
  for (;;) {
    await check();
    const live = await get(read, "karssandbox", "sre", options.namespace);
    const reg = await registration(read);
    if (!live || live.metadata.uid !== created.uid || !reg || reg.metadata.uid !== createdRegistrationUid || canonicalSchema(reg.spec) !== canonicalSchema(spec)) {
      throw new Error("SRE bootstrap identity changed after atomic enrollment");
    }
    const bound = live.spec?.inferenceBudgetRef;
    if (bound) {
      if (canonicalSchema(bound.root) !== canonicalSchema(spec.firstInstallFunding.grant.root)) throw new Error("SRE budget root differs from enrollment");
      const account = await get(read, "karsbudgetaccount", bound.account.name, bound.account.namespace);
      const ledger = account?.status?.ledger;
      if (account && account.metadata.uid === bound.account.uid && !account.metadata.deletionTimestamp
        && account.status?.phase === "Active" && account.status.observedGeneration === account.metadata.generation
        && ledger?.phase === "Active" && ledger.accountUid === bound.account.uid
        && canonicalSchema(ledger.root) === canonicalSchema(spec.firstInstallFunding.grant.root)
        && canonicalSchema(ledger.limits) === canonicalSchema(spec.firstInstallFunding.grant.limits)
        && ledger.nodes?.[created.uid]?.active === true
        && canonicalSchema(ledger.nodes[created.uid].authority.effectiveAuthorization) === canonicalSchema(spec.firstInstallFunding.grant)) {
        if (allowance && ledger.unboundedTokens != null && canonicalSchema(ledger.unboundedTokens) !== canonicalSchema(allowance)) {
          throw new Error("Consumed first-install allowance changed");
        }
        if ("unboundedTokens" in funding && !allowance) {
          allowance = { id: `first-install-${spec.firstInstallFunding.intent.digest}`, configUid: catalog.metadata.uid,
            accountUid: bound.account.uid, root: ledger.root, originalLimits: ledger.limits,
            ledgerDigest: bridgeReactivationDigest(ledger), previousTokenTopUpIds: Object.keys(ledger.tokenTopUps ?? {}).sort() };
          const entries = [...JSON.parse(expectedCatalog.data["unbounded-token-allowances.json"] ?? "[]"), allowance];
          await authorizeUnbounded(async args => {
            if (!["get", "auth", "patch"].includes(args[0])) throw new Error("Unreviewed first-install funding operation");
            if (args[0] === "patch") {
              if (args[1] !== "configmap" || args[2] !== catalog.metadata.name
                || args[3] !== "-n" || args[4] !== options.namespace) throw new Error("Funding escaped the Core catalog");
              await check();
            }
            return (await invoke("kubectl", args, { stdio: "pipe", timeout: 25000 })).stdout;
          }, JSON.stringify(allowance), options.namespace, catalog.metadata.name!);
          expectedCatalog.data["unbounded-token-allowances.json"] = JSON.stringify(entries);
        } else if (!("unboundedTokens" in funding)
          || (canonicalSchema(ledger.unboundedTokens ?? null) === canonicalSchema(allowance)
            && fundingView(ledger).effectiveExpiresAt === null)) {
          return { enrolled: true, revision: 1, sandboxUid: created.uid, registrationUid: reg.metadata.uid, accountUid: bound.account.uid,
            ...(allowance ? { unboundedTokenAllowance: allowance } : {}) };
        }
      }
    }
    await (options.sleep ?? (ms => new Promise(resolve => setTimeout(resolve, ms))))(1000);
  }
}
