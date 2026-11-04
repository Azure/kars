// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { randomBytes } from "node:crypto";
import { constants, chmodSync, closeSync, fchmodSync, fstatSync, fsyncSync, lstatSync, mkdirSync, openSync, readFileSync, readdirSync, realpathSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { gunzipSync } from "node:zlib";
import { parseAllDocuments } from "yaml";
import { at, canonical, CORE_PUBLICATION, CORE_PUBLICATION_FENCE as FENCE, digest, PRIVATE_PREFIX, read, record, reviewed, type Execute } from "./private-activation.js";
import { retirementBinding } from "./private-activation-retirement.js";
import { prepareCoreHelmSchemas, renderCoreSchemaChart } from "./core-helm-schemas.js";
import { coreSchemaDocuments } from "./monitoring-schemas.js";
import { schemaDocuments, type ObjectMap, type SchemaExecute } from "./schema-documents.js";
import {
  advanceRootPublicationBundle, assertRetiredRootPublication, claimRootPublicationLaunch, claimRootPublicationRecovery, commitRootPublication, ownRootPublication, releaseRootPublication,
  type RootMigrationJournal, type RootMigrationReceipt,
} from "./private-root-migration.js";
import { withRootMigrationJournal } from "./private-root-migration-journal.js";
import { privateCommandFailure } from "./private-activation-command-diagnostics.js";
import { assertPublicationWitnessStopped, restorePublicationWitness, stopPublicationWitness } from "./private-publication-witness.js";
import { freezeHistoricalHookFailure, historicalHookProof, verifyHistoricalHookRetirement } from "./private-core-history.js";

export interface PrivateCoreRequest {
  receipt: string; context: string; release: string; namespace: string;
  chart: string; values: string[]; set: string[];
  replacePreparingRequest?: boolean;
  recoverFailedPublication?: boolean;
  retireHistoricalMonitoringHooks?: boolean;
}
const files = ["Chart.yaml", "values.yaml", "templates/publication.yaml", "files/publication.yaml"];
const publicationTemplate = '{{ .Files.Get "files/publication.yaml" }}\n';
const fail = "Protected Core publication changed or lacks complete identity/version fencing; retain its receipt and Pending protection";
type Publication = NonNullable<RootMigrationReceipt["publication"]>;
type Storage = NonNullable<Publication["storage"]>[number] & { object: ObjectMap; status: string };

function key(value: unknown, namespace: string): string {
  return canonical([at(value, "apiVersion"), at(value, "kind"), at(value, "metadata", "namespace") ?? namespace, at(value, "metadata", "name")]);
}
function body(value: unknown): ObjectMap {
  const copy = structuredClone(record(value));
  delete copy.status;
  const meta = record(copy.metadata);
  for (const field of ["uid", "resourceVersion", "generation", "managedFields", "creationTimestamp", "selfLink"]) delete meta[field];
  if (meta.annotations) {
    delete record(meta.annotations)[FENCE];
    delete record(meta.annotations)[CORE_PUBLICATION];
    if (!Object.keys(record(meta.annotations)).length) delete meta.annotations;
  }
  return copy;
}
function staticPolicyBody(value: ObjectMap): ObjectMap {
  const copy = body(value), fields = record(copy.metadata.annotations ?? {});
  for (const field of ["helm.sh/hook", "helm.sh/hook-weight", "helm.sh/hook-delete-policy",
    "meta.helm.sh/release-name", "meta.helm.sh/release-namespace"]) delete fields[field];
  if (!Object.keys(fields).length) delete copy.metadata.annotations;
  return copy;
}
function monitoringInventory(manifest: string, namespace: string): string[] {
  return parseAllDocuments(manifest).flatMap(document => {
    if (document.errors.length) throw new Error(fail);
    return document.contents?.commentBefore?.split("\n").some(line => line.trim().startsWith("Source: kars/charts/monitoring/"))
      ? [key(document.toJSON(), namespace)] : [];
  }).sort();
}
function monitoringConfiguration(release: ObjectMap): unknown {
  const configured = at(release, "config", "monitoring");
  if (configured !== undefined) return configured;
  // Frozen publication values are chart defaults, not Helm's explicit overrides.
  // Accept that provenance only for the exact non-executable snapshot format.
  const chart = record(release.chart ?? {}), templates = chart.templates, assets = chart.files;
  const template = Array.isArray(templates) && templates.length === 1 ? record(templates[0]) : {};
  const asset = Array.isArray(assets) && assets.length === 1 ? record(assets[0]) : {};
  if (canonical(release.config ?? {}) !== canonical({}) || canonical(release.hooks ?? []) !== canonical([])
    || at(chart, "metadata", "name") !== "kars" || at(chart, "metadata", "apiVersion") !== "v2"
    || at(chart, "metadata", "type") !== "application" || typeof at(chart, "metadata", "version") !== "string"
    || canonical(at(chart, "metadata", "dependencies") ?? []) !== canonical([]) || chart.lock || chart.schema
    || template.name !== "templates/publication.yaml"
    || typeof template.data !== "string" || Buffer.from(template.data, "base64").toString("utf8") !== publicationTemplate
    || asset.name !== "files/publication.yaml" || typeof asset.data !== "string"
    || manifestInventory(Buffer.from(asset.data, "base64").toString("utf8")) !== manifestInventory(String(release.manifest))) return undefined;
  return at(chart, "values", "monitoring");
}
function monitoringBootstrapConfiguration(configuration: unknown): unknown {
  if (configuration === undefined) return undefined;
  const copy = structuredClone(record(configuration));
  delete copy.kubeStateMetricsResourcesBaseline;
  if (copy["kube-state-metrics"] !== undefined) delete record(copy["kube-state-metrics"]).resources;
  return copy;
}
function materializeStaticHooks(preview: ObjectMap, prior: Storage[], namespace: string): { staticHooks: ObjectMap[]; monitoring: string[] } {
  let monitoring: string[] = [];
  if (preview.hooks !== undefined && (!Array.isArray(preview.hooks) || preview.hooks.length > 8)) throw new Error(fail);
  const hooks: ObjectMap[] = preview.hooks ?? [];
  const prefix = "kars/charts/monitoring/templates/prometheus-operator/admission-webhooks/job-patch/";
  const bootstrap = hooks.filter(hook => typeof hook.path === "string" && hook.path.startsWith(prefix));
  if (bootstrap.length) {
    const expected: Record<string, string> = { "serviceaccount.yaml": "ServiceAccount", "clusterrole.yaml": "ClusterRole",
      "clusterrolebinding.yaml": "ClusterRoleBinding", "role.yaml": "Role", "rolebinding.yaml": "RoleBinding",
      "job-createSecret.yaml": "Job", "job-patchWebhook.yaml": "Job" };
    const releases = prior.map(entry => storageBody(entry.object).release);
    const baseline = [...releases].reverse().find(release => Array.isArray(release.hooks)
      && release.hooks.some((hook: ObjectMap) => typeof hook.path === "string" && hook.path.startsWith(prefix)));
    const intent = (hook: ObjectMap) => { const value = structuredClone(hook); delete value.last_run; return canonical(value); };
    // These completed bootstrap jobs are not part of the frozen publication.
    // Keep the existing admission configuration; never rerun credential writers.
    monitoring = monitoringInventory(String(preview.manifest), namespace);
    if (!baseline || !monitoring.length
      || canonical(monitoring) !== canonical(monitoringInventory(String(baseline.manifest), namespace))
      || baseline.hooks.filter((hook: ObjectMap) => typeof hook.path === "string" && hook.path.startsWith(prefix)).length !== bootstrap.length
      || releases.slice(releases.indexOf(baseline)).some(release => canonical(monitoringBootstrapConfiguration(monitoringConfiguration(release)) ?? null)
        !== canonical(monitoringBootstrapConfiguration(at(preview, "config", "monitoring")) ?? null))
      || !at(preview, "config", "monitoring")
      || canonical(monitoringBootstrapConfiguration(at(preview, "config", "monitoring")))
        !== canonical(monitoringBootstrapConfiguration(at(baseline, "config", "monitoring")))
      || canonical(monitoringBootstrapConfiguration(at(preview, "config", "monitoring")))
        !== canonical(monitoringBootstrapConfiguration(monitoringConfiguration(releases.at(-1)!)))
      || canonical(bootstrap.map(hook => hook.path.slice(prefix.length)).sort()) !== canonical(Object.keys(expected).sort())
      || bootstrap.some(hook => {
        const documents = schemaDocuments(String(hook.manifest));
        const previous = baseline.hooks.filter((value: ObjectMap) => value.path === hook.path);
        return documents.length !== 1 || documents[0]!.kind !== expected[hook.path.slice(prefix.length)]
          || hook.kind !== documents[0]!.kind || previous.length !== 1 || at(previous[0], "last_run", "phase") !== "Succeeded"
          || intent(previous[0]) !== intent(hook);
      })) throw new Error("Protected Core can omit only the unchanged successful monitoring admission bootstrap; executable or unknown hooks remain refused");
  }
  const staticHooks = hooks.filter(hook => !bootstrap.includes(hook));
  if (staticHooks.length > 1) throw new Error(fail);
  return { monitoring, staticHooks: staticHooks.map((hook: ObjectMap) => {
    const documents = schemaDocuments(String(hook.manifest));
    const object = documents[0];
    if (documents.length !== 1 || object?.apiVersion !== "kars.azure.com/v1alpha1" || object.kind !== "ToolPolicy"
      || object.metadata.name !== "kars-default" || object.metadata.namespace !== namespace
      || at(object, "metadata", "annotations", "helm.sh/hook") !== "post-install,post-upgrade") {
      throw new Error("Protected Core rejects executable or unknown hooks; only the existing unchanged kars-default ToolPolicy can become a fenced ordinary resource");
    }
    const predecessors = prior.flatMap(entry => {
      const release = storageBody(entry.object).release;
      return [...schemaDocuments(String(release.manifest)), ...(release.hooks ?? []).flatMap((previous: ObjectMap) =>
        at(previous, "last_run", "phase") === "Succeeded" ? schemaDocuments(String(previous.manifest)) : [])];
    });
    if (!predecessors.some(previous => key(previous, namespace) === key(object, namespace)
      && canonical(staticPolicyBody(previous)) === canonical(staticPolicyBody(object)))) throw new Error("Default ToolPolicy has no unchanged successful release predecessor");
    return object;
  }) };
}
function immutable(value: ObjectMap, namespace: string): boolean {
  return !["Deployment", "DaemonSet", "StatefulSet", "Service", "Ingress", "HorizontalPodAutoscaler", "PodDisruptionBudget"].includes(value.kind)
    || ((value.metadata.namespace ?? namespace) === namespace
      && !(value.kind === "Deployment" && value.metadata.name === "kars-controller"));
}
function storageBody(object: unknown): { release: ObjectMap; fingerprint: string } {
  const encoded = at(object, "data", "release");
  if (typeof encoded !== "string" || encoded.length > 2_097_152) throw new Error(fail);
  const binary = Buffer.from(Buffer.from(encoded, "base64").toString("utf8"), "base64");
  const raw = binary[0] === 0x1f && binary[1] === 0x8b ? gunzipSync(binary, { maxOutputLength: 16_777_216 }) : binary;
  const release = record(JSON.parse(raw.toString("utf8")));
  const copy = structuredClone(release);
  delete record(copy.info).status;
  return { release, fingerprint: digest(copy) };
}
async function readStorage(run: Execute, request: PrivateCoreRequest): Promise<Storage[]> {
  const result = record(JSON.parse(await run(["get", "secrets", "-n", request.namespace,
    "-l", `owner=helm,name=${request.release}`, "--chunk-size=0", "-o", "json"])));
  if (!Array.isArray(result.items) || !result.items.length || result.items.length > 1024 || at(result, "metadata", "continue")) throw new Error(fail);
  const entries = result.items.map(object => {
    const id = reviewed(object), { release, fingerprint } = storageBody(object);
    const revision = release.version;
    if (release.name !== request.release || release.namespace !== request.namespace || !Number.isSafeInteger(revision)
      || Number(revision) < 1 || at(object, "type") !== "helm.sh/release.v1"
      || id.name !== `sh.helm.release.v1.${request.release}.v${revision}`
      || at(object, "metadata", "labels", "version") !== String(revision)
      || at(object, "metadata", "labels", "status") !== at(release, "info", "status")) throw new Error(fail);
    return { name: id.name, uid: id.uid, revision: Number(revision), digest: fingerprint,
      status: String(at(release, "info", "status")), object: record(object) };
  }).sort((a, b) => a.revision - b.revision);
  if (entries.some((entry, index) => entry.revision !== index + 1)) throw new Error(fail);
  return entries;
}
function retainedFailure(entry: Storage, receipt?: RootMigrationReceipt): boolean {
  return entry.status === "failed" && entry.object.immutable === true && !!receipt?.failedPublications?.some(publication => {
    const recorded = publication.recovery?.storage.at(-1);
    return publication.recovery?.fenced === true && publication.revision === entry.revision
      && canonical(recorded) === canonical(savedStorage([entry])[0]);
  });
}
async function successfulHistory(run: Execute, entries: Storage[], receipt?: RootMigrationReceipt): Promise<void> {
  if (!entries.length) throw new Error(fail);
  const history = entries.map(entry => ({ ...entry, release: storageBody(entry.object).release }));
  for (const [index, entry] of entries.entries()) {
    if (index === entries.length - 1 ? entry.status === "deployed"
      : entry.status === "superseded" || (entry.status === "deployed" && entry.object.immutable === true)) continue;
    if (retainedFailure(entry, receipt)) continue;
    if (entry.status === "failed" && await verifyHistoricalHookRetirement(run, history[index]!, history)) continue;
    throw new Error("Protected Core requires complete successful release history; failed/pending records never prove writer termination");
  }
}
async function storage(run: Execute, request: PrivateCoreRequest, receipt?: RootMigrationReceipt): Promise<Storage[]> {
  const entries = await readStorage(run, request);
  await successfulHistory(run, entries, receipt);
  return entries;
}

async function retireHistoricalMonitoringHooks(
  run: Execute, request: PrivateCoreRequest, receipt: RootMigrationReceipt, journal: RootMigrationJournal,
): Promise<void> {
  if (receipt.phase !== "rotation-required" || receipt.publication
    || request.release !== receipt.plan.review.release.name || request.namespace !== receipt.plan.review.release.namespace) {
    throw new Error("Historical hook retirement requires the reviewed release and an unpublished retired root");
  }
  await assertRetiredRootPublication(run, receipt, false);
  const entries = await readStorage(run, request);
  const history = entries.map(entry => ({ ...entry, release: storageBody(entry.object).release }));
  if (entries.at(-1)?.status !== "deployed" || entries.some(entry => !["deployed", "superseded", "failed"].includes(entry.status))) throw new Error(fail);
  const failures = history.filter(entry => entry.status === "failed" && !retainedFailure(entry, receipt));
  if (!failures.length) throw new Error("No historical hook failure requires retirement");
  const proofs = failures.map(entry => historicalHookProof(entry, history));
  const saved = savedStorage(entries);
  if (receipt.historicalHookRetirement) {
    if (canonical(receipt.historicalHookRetirement.storage) !== canonical(saved)
      || canonical(receipt.historicalHookRetirement.proofs) !== canonical(proofs)) throw new Error(fail);
  } else {
    receipt.historicalHookRetirement = { storage: saved, proofs, phase: "prepared" };
    await journal.save(receipt);
  }
  for (const [index, entry] of failures.entries()) {
    if (!await verifyHistoricalHookRetirement(run, entry, history)) await freezeHistoricalHookFailure(run, entry, proofs[index]!);
  }
  await assertRetiredRootPublication(run, receipt, false);
  const after = await storage(run, request, receipt);
  if (canonical(savedStorage(after)) !== canonical(saved)) throw new Error(fail);
  receipt.historicalHookRetirement.phase = "fenced";
  await journal.save(receipt);
}
function savedStorage(entries: Storage[]): NonNullable<Publication["storage"]> {
  return entries.map(({ object: _object, status: _status, ...entry }) => entry);
}
function syncDirectory(path: string): void {
  const fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try { fsyncSync(fd); } finally { closeSync(fd); }
}
function artifactDigest(path: string): string {
  if (realpathSync(path) !== resolve(path)) throw new Error(fail);
  for (const [directory, expected] of [[".", ["Chart.yaml", "files", "templates", "values.yaml"]],
    ["templates", ["publication.yaml"]], ["files", ["publication.yaml"]]] as const) {
    const target = join(path, directory), info = lstatSync(target);
    if (!info.isDirectory() || info.uid !== process.getuid?.() || (info.mode & 0o777) !== 0o500
      || canonical(readdirSync(target).sort()) !== canonical([...expected].sort())) throw new Error(fail);
  }
  const values = files.map(file => {
    const fd = openSync(join(path, file), constants.O_RDONLY | constants.O_NOFOLLOW);
    try {
      const info = fstatSync(fd), named = lstatSync(join(path, file));
      if (!info.isFile() || info.dev !== named.dev || info.ino !== named.ino || info.nlink !== 1
        || (info.mode & 0o777) !== 0o400 || info.uid !== process.getuid?.() || info.size > 16_777_216) throw new Error(fail);
      return readFileSync(fd, "utf8");
    } finally { closeSync(fd); }
  });
  return digest(values);
}
function snapshot(receipt: string, attempt: string, preview: ObjectMap, documents: ObjectMap[]): { artifact: string; digest: string } {
  const artifact = resolve(`${receipt}.core-${attempt}-${randomBytes(8).toString("hex")}`);
  mkdirSync(artifact, { mode: 0o700 });
  mkdirSync(join(artifact, "templates"), { mode: 0o700 });
  mkdirSync(join(artifact, "files"), { mode: 0o700 });
  const metadata = record(at(preview, "chart", "metadata"));
  if (metadata.name !== "kars" || typeof metadata.version !== "string") throw new Error("Publication requires the maintained kars Core chart");
  const contents = [
    JSON.stringify({ apiVersion: "v2", name: metadata.name, version: metadata.version, type: "application",
      ...(metadata.appVersion ? { appVersion: metadata.appVersion } : {}) }),
    JSON.stringify(preview.config ?? {}),
    publicationTemplate,
    documents.map(value => JSON.stringify(value)).join("\n---\n") + "\n",
  ];
  for (const [index, file] of files.entries()) {
    const fd = openSync(join(artifact, file), constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600);
    try { writeFileSync(fd, contents[index]!); fchmodSync(fd, 0o400); fsyncSync(fd); } finally { closeSync(fd); }
  }
  for (const dir of ["templates", "files", "."]) {
    chmodSync(join(artifact, dir), 0o500);
    syncDirectory(join(artifact, dir));
  }
  syncDirectory(dirname(artifact));
  return { artifact, digest: artifactDigest(artifact) };
}
function ownership(document: ObjectMap, request: PrivateCoreRequest): ObjectMap {
  const desired = structuredClone(document);
  desired.metadata.labels = { ...desired.metadata.labels, "app.kubernetes.io/managed-by": "Helm" };
  desired.metadata.annotations = { ...desired.metadata.annotations,
    "meta.helm.sh/release-name": request.release, "meta.helm.sh/release-namespace": request.namespace };
  return desired;
}
function retainedStaticPolicy(document: ObjectMap, live: ObjectMap): ObjectMap {
  const desired = structuredClone(document);
  if (desired.metadata.finalizers === undefined
    && canonical(live.metadata.finalizers ?? []) === canonical(["kars.azure.com/toolpolicy-cleanup"])) {
    desired.metadata.finalizers = structuredClone(live.metadata.finalizers);
  }
  const before = at(live, "spec", "agtProfile", "inline"), after = at(desired, "spec", "agtProfile", "inline");
  if (typeof before === "string" && typeof after === "string" && before === `${after}\n`) {
    const current = parseAllDocuments(before), rendered = parseAllDocuments(after);
    // Helm trims a hook's final newline. Preserve the live policy bytes and
    // digest only when that newline does not change any YAML scalar value.
    if (current.length === 1 && rendered.length === 1 && !current[0]!.errors.length && !rendered[0]!.errors.length
      && canonical(current[0]!.toJSON()) === canonical(rendered[0]!.toJSON())) desired.spec.agtProfile.inline = before;
  }
  return desired;
}
async function adoptStaticPolicy(
  run: Execute, request: PrivateCoreRequest, receipt: RootMigrationReceipt, journal: RootMigrationJournal, document: ObjectMap,
): Promise<void> {
  const publication = receipt.publication!;
  if (publication.phase !== "preparing" || document.apiVersion !== "kars.azure.com/v1alpha1"
    || document.kind !== "ToolPolicy" || document.metadata.name !== "kars-default" || document.metadata.namespace !== request.namespace) throw new Error(fail);
  let live = await read(run, "ToolPolicy", "kars-default", request.namespace);
  const desired = retainedStaticPolicy(document, live);
  if (canonical(staticPolicyBody(live)) !== canonical(staticPolicyBody(desired))
    || at(live, "metadata", "labels", "app.kubernetes.io/managed-by") !== "Helm") throw new Error("Static policy adoption requires its unchanged live predecessor");
  Object.assign(document, desired);
  const fields = record(at(live, "metadata", "annotations"));
  const ownershipFields = { "meta.helm.sh/release-name": request.release, "meta.helm.sh/release-namespace": request.namespace };
  if (Object.entries(ownershipFields).some(([field, value]) => fields[field] !== undefined && fields[field] !== value)) {
    throw new Error("Protected publication cannot adopt unrelated resources");
  }
  if (!publication.adoption) {
    if (Object.entries(ownershipFields).every(([field, value]) => fields[field] === value)) return;
    const id = reviewed(live);
    publication.adoption = { uid: id.uid, resourceVersion: id.resourceVersion, annotations: structuredClone(fields),
      beforeDigest: digest(body(live)), afterDigest: digest(body(ownership(live, request))) };
    await journal.save(receipt);
  }
  const intent = publication.adoption;
  if (reviewed(live).uid !== intent.uid) throw new Error("Static policy adoption identity changed");
  if (intent.appliedResourceVersion) {
    if (reviewed(live).resourceVersion !== intent.appliedResourceVersion || digest(body(live)) !== intent.afterDigest) throw new Error("Journaled static policy adoption changed");
    return;
  }
  if (reviewed(live).resourceVersion === intent.resourceVersion && digest(body(live)) === intent.beforeDigest) {
    const result = record(JSON.parse(await run(["patch", "ToolPolicy", "kars-default", "-n", request.namespace,
      "--type=json", "--patch-file=/dev/stdin", "-o", "json"], JSON.stringify([
      { op: "test", path: "/metadata/uid", value: intent.uid },
      { op: "test", path: "/metadata/resourceVersion", value: intent.resourceVersion },
      { op: "test", path: "/metadata/annotations", value: intent.annotations },
      { op: "add", path: "/metadata/annotations", value: { ...intent.annotations, ...ownershipFields } },
    ]))));
    if (reviewed(result).uid !== intent.uid || reviewed(result).resourceVersion === intent.resourceVersion
      || digest(body(result)) !== intent.afterDigest) throw new Error("Static policy adoption acknowledgement differs");
    live = await read(run, "ToolPolicy", "kars-default", request.namespace);
    if (reviewed(live).resourceVersion !== reviewed(result).resourceVersion) throw new Error("Static policy changed after adoption");
  }
  // An identical metadata-only retry uses the original RV. Once the desired
  // ownership is observed at a new RV, every delayed copy of that CAS is stale.
  if (reviewed(live).uid !== intent.uid || reviewed(live).resourceVersion === intent.resourceVersion
    || digest(body(live)) !== intent.afterDigest
    || Object.entries(ownershipFields).some(([field, value]) => at(live, "metadata", "annotations", field) !== value)) {
    throw new Error("Static policy adoption cannot recover an unrelated live change");
  }
  intent.appliedResourceVersion = reviewed(live).resourceVersion;
  await journal.save(receipt);
}

function monitoringQuantity(value: unknown, resource: string): bigint | undefined {
  if (typeof value !== "string") return undefined;
  const match = value.match(resource === "cpu" ? /^([1-9][0-9]{0,8})(m?)$/ : /^([1-9][0-9]{0,8})(Mi|Gi|Ti)$/);
  if (!match) return undefined;
  const scale = resource === "cpu" ? (match[2] === "m" ? 1n : 1000n)
    : match[2] === "Mi" ? 1n : match[2] === "Gi" ? 1024n : 1048576n;
  return BigInt(match[1]!) * scale;
}

function monitoringCapacityOnly(desired: ObjectMap, current: ObjectMap, configured: unknown): boolean {
  if (desired.apiVersion !== "apps/v1" || desired.kind !== "Deployment"
    || at(current, "metadata", "labels", "app.kubernetes.io/name") !== "kube-state-metrics"
    || at(current, "spec", "selector", "matchLabels", "app.kubernetes.io/name") !== "kube-state-metrics") return false;
  const containers = at(desired, "spec", "template", "spec", "containers"), previous = at(current, "spec", "template", "spec", "containers");
  if (!Array.isArray(containers) || containers.length !== 1 || !Array.isArray(previous) || previous.length !== 1
    || at(containers[0], "name") !== "kube-state-metrics" || at(previous[0], "name") !== "kube-state-metrics") return false;
  const resources = record(configured ?? {}), projected = record(at(containers[0], "resources") ?? {});
  if ([resources, projected].some(value => canonical(Object.keys(value).sort()) !== canonical(["limits", "requests"]))
    || ["limits", "requests"].some(group => {
      const quantities = record(resources[group]), actual = record(projected[group]);
      return [quantities, actual].some(value => canonical(Object.keys(value).sort()) !== canonical(["cpu", "memory"]))
        || ["cpu", "memory"].some(resource => {
          const expected = monitoringQuantity(quantities[resource], resource);
          // Kubernetes canonicalizes 1000m to 1 and 1024Mi to 1Gi in its projection.
          return expected === undefined || monitoringQuantity(actual[resource], resource) !== expected;
        });
    })) return false;
  // The server dry-run validates quantities. Only the requested bounded resources
  // may differ; probes, credentials, images and every other field stay unchanged.
  const retained = at(previous[0], "resources");
  if (retained === undefined) return false;
  record(containers[0]).resources = retained;
  return canonical(desired) === canonical(current);
}
function unchangedBody(projected: ObjectMap, live: ObjectMap, receipt: RootMigrationReceipt, monitoringResources?: unknown): boolean {
  const revision = receipt.publication!.storage!.length;
  const desired = body(projected), current = body(live);
  if (monitoringResources !== undefined && monitoringCapacityOnly(structuredClone(desired), current, monitoringResources)) return true;
  if (projected.kind === "ConfigMap" && projected.metadata.name === "kars-datapath-witness-settings"
    && at(live, "metadata", "labels", "kars.azure.com/witness-addon") === "true") {
    const before = record(JSON.parse(String(at(live, "data", "settings.json"))));
    const after = record(JSON.parse(String(at(projected, "data", "settings.json"))));
    const failed = new Set(receipt.failedPublications?.filter(value => value.recovery?.fenced).map(value => value.revision));
    const previousRevision = before.release_revision;
    if (typeof previousRevision !== "number" || !Number.isSafeInteger(previousRevision) || previousRevision < 1 || previousRevision > revision
      || Array.from({ length: revision - previousRevision }, (_, index) => previousRevision + index + 1).some(value => !failed.has(value))
      || before.schema_version !== 1 || after.release_revision !== revision + 1
      || canonical({ ...after, release_revision: before.release_revision }) !== canonical(before)) return false;
    // A failed publication may leave the previous settings intact. Only fenced
    // failed revisions may be skipped; capture scope and configuration stay pinned.
    desired.data["settings.json"] = current.data["settings.json"];
  }
  return canonical(desired) === canonical(current);
}

async function prepareDocuments(
  execute: SchemaExecute, run: Execute, request: PrivateCoreRequest, receipt: RootMigrationReceipt, documents: ObjectMap[], previous: ObjectMap[],
  staticHooks: ObjectMap[], monitoring: string[], monitoringResources: unknown,
): Promise<{ documents: ObjectMap[]; resources: NonNullable<Publication["resources"]> }> {
  const ids = documents.map(object => key(object, request.namespace));
  if (!ids.length || ids.length > 512 || new Set(ids).size !== ids.length || previous.some(object => !ids.includes(key(object, request.namespace)))) {
    throw new Error("Protected Core publication cannot remove resources or use duplicate/incomplete inventories");
  }
  const resources: NonNullable<Publication["resources"]> = [], output: ObjectMap[] = [];
  for (const document of documents) {
    const staticHook = staticHooks.includes(document);
    if ((!staticHook && at(document, "metadata", "annotations", "helm.sh/hook") !== undefined) || ["Pod", "Job", "CronJob", "List"].includes(document.kind)) {
      throw new Error("Protected Core publication cannot own executable hooks or one-shot writers; provide a hook-free Core upgrade render");
    }
    const live = await read(run, document.kind, document.metadata.name, document.metadata.namespace ?? request.namespace);
    const namespace = at(live, "metadata", "namespace");
    if (namespace !== undefined && (typeof namespace !== "string" || !namespace)) throw new Error(fail);
    if (at(live, "metadata", "labels", "app.kubernetes.io/managed-by") !== "Helm"
      || [["meta.helm.sh/release-name", request.release], ["meta.helm.sh/release-namespace", request.namespace]]
        .some(([field, value]) => at(live, "metadata", "annotations", field!) !== value)) {
      throw new Error("Protected publication cannot adopt unrelated resources");
    }
    const desired = ownership(document, request);
    if (namespace) desired.metadata.namespace = namespace;
    if (staticHook) {
      for (const field of ["helm.sh/hook", "helm.sh/hook-weight", "helm.sh/hook-delete-policy"]) delete desired.metadata.annotations[field];
    }
    Object.assign(desired.metadata, { uid: reviewed(live).uid, resourceVersion: reviewed(live).resourceVersion });
    if (desired.kind === "Namespace" && desired.metadata.name === request.namespace) {
      desired.metadata.annotations[CORE_PUBLICATION] = at(live, "metadata", "annotations", CORE_PUBLICATION);
    }
    const projected = record(JSON.parse((await execute("kubectl", ["apply", "--server-side", "--field-manager=helm",
      "--dry-run=server", "--namespace", namespace ?? request.namespace, "-f", "-", "-o", "json"], { stdio: "pipe", input: JSON.stringify(desired) })).stdout));
    if (reviewed(projected).uid !== reviewed(live).uid
      || ((immutable(desired, request.namespace) || monitoring.includes(key(desired, request.namespace)))
        && (staticHook ? canonical(staticPolicyBody(projected)) !== canonical(staticPolicyBody(live))
          : !unchangedBody(projected, live, receipt, monitoring.includes(key(desired, request.namespace)) ? monitoringResources : undefined)))) {
      throw new Error(`Protected credentials, TLS data, policy, schemas, accounting and root-namespace companions must remain semantically unchanged (${desired.kind}/${desired.metadata.name})`);
    }
    if (desired.kind === "Deployment" && desired.metadata.name === "kars-controller") {
      if (namespace !== request.namespace || reviewed(live).uid !== receipt.plan.review.activation.root.deployment.uid
        || canonical(projected.spec) !== canonical({ ...receipt.plan.targetSpec, replicas: 0 })) throw new Error(fail);
    }
    const writer = receipt.publication!.witnessWriter;
    if (writer && desired.kind === "Deployment" && desired.metadata.name === at(writer.deployment, "metadata", "name")
      && namespace === at(writer.deployment, "metadata", "namespace")) writer.publishedSpec = structuredClone(record(projected.spec));
    const metrics = desired.kind === "Deployment"
      && at(projected, "metadata", "labels", "app.kubernetes.io/name") === "kube-state-metrics"
      && at(projected, "spec", "selector", "matchLabels", "app.kubernetes.io/name") === "kube-state-metrics";
    const deploymentRevision = at(projected, "metadata", "annotations", "deployment.kubernetes.io/revision");
    if (metrics && deploymentRevision !== undefined && typeof deploymentRevision !== "string") throw new Error(fail);
    resources.push({ kind: desired.kind, name: desired.metadata.name, ...(namespace ? { namespace } : {}),
      uid: reviewed(live).uid, digest: digest(body(projected)),
      ...(metrics ? { deploymentRevision: typeof deploymentRevision === "string" ? deploymentRevision : null } : {}) });
    output.push(desired);
  }
  if (!output.some(value => value.kind === "Deployment" && value.metadata.name === "kars-controller" && value.metadata.namespace === request.namespace)) throw new Error(fail);
  return { documents: output, resources };
}

async function waitForPublishedResources(run: Execute, receipt: RootMigrationReceipt, documents: ObjectMap[]): Promise<void> {
  const root = receipt.plan.review.activation.root;
  const deadline = Date.now() + 300_000;
  const wait = async (args: string[]) => {
    const remaining = deadline - Date.now();
    if (remaining <= 0) throw new Error("Published non-root resources did not become Ready; retain publication ownership");
    await run([...args, `--timeout=${remaining}ms`]);
  };
  for (const document of documents) {
    const kind = document.kind, name = document.metadata.name;
    const namespace = document.metadata.namespace ?? receipt.publication!.namespace;
    if (kind === "Deployment" && namespace === root.namespace.name && name === root.deployment.name) {
      // Pending admission deliberately denies new root ReplicaSets, even at zero.
      // Root/companion execution is qualified and required Ready by finalize.
      // The reviewed spec is server-projected; the frozen template omits defaults.
      const live = await read(run, kind, name, namespace);
      if (document.metadata.uid !== root.deployment.uid || document.spec?.replicas !== 0
        || reviewed(live).uid !== root.deployment.uid
        || canonical(live.spec) !== canonical({ ...receipt.plan.targetSpec, replicas: 0 })) throw new Error(fail);
      continue;
    }
    let args: string[] | undefined;
    if (["Deployment", "DaemonSet", "StatefulSet"].includes(kind)) {
      if (kind === "Deployment" && at(document, "spec", "paused") === true) continue;
      if (at(document, "spec", "updateStrategy", "type") === "OnDelete") {
        const live = await read(run, kind, name, namespace);
        args = ["wait", `${kind}/${name}`, "-n", namespace,
          `--for=jsonpath={.status.observedGeneration}=${at(live, "metadata", "generation")}`];
      } else args = ["rollout", "status", `${kind}/${name}`, "-n", namespace];
    } else if (kind === "PersistentVolumeClaim") {
      args = ["wait", `${kind}/${name}`, "-n", namespace, "--for=jsonpath={.status.phase}=Bound"];
    } else if (kind === "CustomResourceDefinition") {
      args = ["wait", `${kind}/${name}`, "--for=condition=Established"];
    } else if (kind === "Service" && at(document, "spec", "type") !== "ExternalName") {
      await wait(["wait", `${kind}/${name}`, "-n", namespace, "--for=jsonpath={.spec.clusterIP}"]);
      if (at(document, "spec", "type") === "LoadBalancer" && !(at(document, "spec", "externalIPs") as unknown[] | undefined)?.length) {
        args = ["wait", `${kind}/${name}`, "-n", namespace, "--for=jsonpath={.status.loadBalancer.ingress}"];
      }
    } else if (["ReplicaSet", "ReplicationController"].includes(kind)) {
      const live = await read(run, kind, name, namespace);
      await wait(["wait", `${kind}/${name}`, "-n", namespace,
        `--for=jsonpath={.status.observedGeneration}=${at(live, "metadata", "generation")}`]);
      const replicas = at(document, "spec", "replicas") ?? 1;
      if (replicas !== 0) args = ["wait", `${kind}/${name}`, "-n", namespace, `--for=jsonpath={.status.readyReplicas}=${replicas}`];
    }
    if (args) await wait(args);
  }
}

function manifestInventory(manifest: string): string {
  const documents = schemaDocuments(manifest), inventory = new Map<string, ObjectMap>();
  for (const object of documents) {
    const id = key(object, "");
    if (inventory.has(id)) throw new Error("Published Helm manifest differs from the owned snapshot: duplicate resource");
    inventory.set(id, object);
  }
  if (!inventory.size || inventory.size > 512) throw new Error("Published Helm manifest differs from the owned snapshot: incomplete inventory");
  return canonical([...inventory.entries()].sort(([left], [right]) => left.localeCompare(right)));
}

async function verifyStorage(run: Execute, request: PrivateCoreRequest, receipt: RootMigrationReceipt): Promise<Storage[]> {
  const publication = receipt.publication!, current = await storage(run, request, receipt), before = publication.storage!;
  if (current.length !== before.length + 1 || current.at(-1)?.revision !== publication.revision
    || canonical(savedStorage(current.slice(0, -1))) !== canonical(before)) throw new Error("Core release history, storage UID or retained revision content changed");
  const newest = storageBody(current.at(-1)!.object).release;
  if (manifestInventory(String(newest.manifest)) !== manifestInventory(readFileSync(join(publication.artifact!, "files/publication.yaml"), "utf8"))
    || newest.apply_method !== "ssa" || (newest.hooks !== undefined && (!Array.isArray(newest.hooks) || newest.hooks.length))) {
    throw new Error("Published Helm manifest differs from the owned snapshot");
  }
  return current;
}
async function verifyResources(run: Execute, receipt: RootMigrationReceipt): Promise<void> {
  for (const expected of receipt.publication!.resources!) {
    const actual = await read(run, expected.kind, expected.name, expected.namespace);
    const projected = body(actual);
    for (const transition of [...(receipt.bundleTransitions ?? [])].reverse()) {
      if (transition.publication !== receipt.publication!.attempt || actual.kind !== "Namespace"
        || reviewed(transition.namespace).uid !== reviewed(actual).uid) continue;
      const fields = record(projected.metadata.annotations), revision = `${PRIVATE_PREFIX}bundle-revision`, retirement = `${PRIVATE_PREFIX}root-retirement`;
      const binding = retirementBinding({ ...transition.activation, bundleRevision: transition.review.revision });
      if (fields[revision] === transition.review.revision && fields[retirement] === canonical({ ...transition.retirement, binding })) {
        fields[revision] = at(transition.namespace, "metadata", "annotations", revision)!;
        fields[retirement] = at(transition.namespace, "metadata", "annotations", retirement)!;
      }
    }
    const revision = "deployment.kubernetes.io/revision";
    if (digest(projected) !== expected.digest && actual.kind === "Deployment") {
      const writer = receipt.publication!.witnessWriter?.deployment;
      const isWriter = writer && reviewed(writer).uid === expected.uid;
      const before = isWriter ? at(writer, "metadata", "annotations", revision) : expected.deploymentRevision ?? undefined;
      const after = at(actual, "metadata", "annotations", revision);
      const allowed = isWriter || (Object.hasOwn(expected, "deploymentRevision")
        && at(actual, "metadata", "labels", "app.kubernetes.io/name") === "kube-state-metrics"
        && at(actual, "spec", "selector", "matchLabels", "app.kubernetes.io/name") === "kube-state-metrics");
      // The stopped witness and resource-adjusted metrics Deployment can acquire
      // new ReplicaSets. Every other projected field and UID stays pinned.
      if (allowed && typeof after === "string" && /^[1-9][0-9]*$/.test(after)
        && (before === undefined || (typeof before === "string" && /^[1-9][0-9]*$/.test(before) && BigInt(after) >= BigInt(before)))) {
        const fields = record(projected.metadata.annotations);
        if (before === undefined) delete fields[revision];
        else fields[revision] = before;
        if (!Object.keys(fields).length) delete projected.metadata.annotations;
      }
    }
    if (reviewed(actual).uid !== expected.uid || digest(projected) !== expected.digest) throw new Error(fail);
  }
}
async function fence(run: Execute, kind: string, object: ObjectMap, attempt: string, releaseStorage = false): Promise<void> {
  const id = reviewed(object), annotations = { ...record(at(object, "metadata", "annotations") ?? {}), [FENCE]: attempt };
  const result = record(JSON.parse(await run(["patch", kind, id.name,
    ...(object.metadata.namespace ? ["-n", object.metadata.namespace] : []), "--type=json", "--patch-file=/dev/stdin", "-o", "json"],
  JSON.stringify([{ op: "test", path: "/metadata/uid", value: id.uid },
    { op: "test", path: "/metadata/resourceVersion", value: id.resourceVersion },
    { op: "add", path: "/metadata/annotations", value: annotations },
    ...(releaseStorage ? [{ op: "add", path: "/immutable", value: true }] : [])]))));
  if (reviewed(result).uid !== id.uid || reviewed(result).resourceVersion === id.resourceVersion
    || at(result, "metadata", "annotations", FENCE) !== attempt || (releaseStorage && result.immutable !== true)) throw new Error(fail);
}

async function fencePublicationWitness(run: Execute, receipt: RootMigrationReceipt, journal: RootMigrationJournal, documents: ObjectMap[]): Promise<void> {
  const publication = receipt.publication!, writer = publication.witnessWriter;
  if (!writer) return;
  await assertPublicationWitnessStopped(run, receipt);
  const desired = documents.find(value => value.kind === "ConfigMap" && value.metadata.name === "kars-datapath-witness"
    && at(value, "metadata", "labels", "kars.azure.com/witness-addon") === "true");
  if (!desired) throw new Error(fail);
  let live = await read(run, "ConfigMap", desired.metadata.name, desired.metadata.namespace);
  if (at(live, "metadata", "labels", "app.kubernetes.io/managed-by") !== "Helm"
    || at(live, "metadata", "annotations", "meta.helm.sh/release-name") !== publication.release
    || at(live, "metadata", "annotations", "meta.helm.sh/release-namespace") !== publication.namespace) throw new Error(fail);
  if (!writer.evidenceFence) {
    writer.evidenceFence = { uid: reviewed(live).uid, resourceVersion: reviewed(live).resourceVersion };
    await journal.save(receipt);
  }
  const before = writer.evidenceFence, pauseFence = `${publication.attempt}:witness-paused`;
  if (reviewed(live).uid !== before.uid) throw new Error(fail);
  if (reviewed(live).resourceVersion === before.resourceVersion) await fence(run, "ConfigMap", live, pauseFence);
  live = await read(run, "ConfigMap", desired.metadata.name, desired.metadata.namespace);
  // Even after the last Pod exits, an already-sent witness PUT may still be in
  // flight. Its old RV must be invalidated before freezing the Helm snapshot.
  if (reviewed(live).uid !== before.uid || reviewed(live).resourceVersion === before.resourceVersion
    || at(live, "metadata", "annotations", FENCE) !== pauseFence) throw new Error(fail);
}

async function recoverFailedPublication(
  run: Execute, request: PrivateCoreRequest, receipt: RootMigrationReceipt, journal: RootMigrationJournal, requestDigest: string,
): Promise<RootMigrationReceipt> {
  const publication = receipt.publication;
  if (receipt.phase !== "rotation-required" || !publication || !["publishing", "succeeded"].includes(publication.phase)
    || publication.context !== request.context || publication.release !== request.release || publication.namespace !== request.namespace
    || !publication.artifact || artifactDigest(publication.artifact) !== publication.digest
    || !publication.storage?.length || !publication.resources?.length || publication.revision !== publication.storage.length + 1
    || (publication.recovery && publication.recovery.requestDigest !== requestDigest)) throw new Error(fail);
  const frozen = schemaDocuments(readFileSync(join(publication.artifact, "files/publication.yaml"), "utf8"));
  manifestInventory(readFileSync(join(publication.artifact, "files/publication.yaml"), "utf8"));
  if (frozen.length !== publication.resources.length || frozen.some(object => {
    const expected = publication.resources!.filter(resource => resource.kind === object.kind && resource.name === object.metadata.name
      && resource.namespace === object.metadata.namespace);
    return expected.length !== 1 || expected[0]!.uid !== reviewed(object).uid
      || ["Pod", "Job", "CronJob", "List"].includes(object.kind) || at(object, "metadata", "annotations", "helm.sh/hook") !== undefined;
  })) throw new Error(fail);
  const failedStorage = async (): Promise<Storage[]> => {
    const current = await readStorage(run, request), latest = current.at(-1);
    if (current.length !== publication.revision || !latest || !["failed", "deployed"].includes(latest.status)
      || canonical(savedStorage(current.slice(0, -1))) !== canonical(publication.storage)
      || (publication.recovery && canonical(savedStorage(current)) !== canonical(publication.recovery.storage))) {
      throw new Error("Recovery requires the exact terminal revision and complete retained history; pending, absent and unknown outcomes remain refused");
    }
    if (publication.phase === "succeeded" && (latest.status !== "deployed"
      || current.some(entry => entry.object.immutable !== true))) throw new Error(fail);
    await successfulHistory(run, latest.status === "deployed" ? current : current.slice(0, -1), receipt);
    const release = storageBody(latest.object).release;
    if (release.apply_method !== "ssa" || (release.hooks !== undefined && (!Array.isArray(release.hooks) || release.hooks.length))
      || manifestInventory(String(release.manifest)) !== manifestInventory(readFileSync(join(publication.artifact!, "files/publication.yaml"), "utf8"))) throw new Error(fail);
    if (latest.status === "deployed") {
      // A terminal Helm status alone does not prove successful publication.
      // Require the complete original projection before fencing a fresh successor.
      await assertPublicationWitnessStopped(run, receipt);
      await verifyResources(run, receipt);
    }
    return current;
  };
  const before = await failedStorage();
  if (!publication.recovery) {
    publication.recovery = { attempt: randomBytes(32).toString("hex"), requestDigest, storage: savedStorage(before) };
    await journal.save(receipt);
  }
  if (receipt.bundleTransitions?.at(-1)?.applied === undefined && receipt.bundleTransitions?.length) {
    receipt = await advanceRootPublicationBundle(run, receipt, journal, frozen);
  }
  receipt = await claimRootPublicationRecovery(run, receipt, journal, frozen);
  const marker = `${publication.recovery.attempt}:failed-publication`;
  const resource = async (object: ObjectMap): Promise<ObjectMap> => {
    const live = await read(run, object.kind, object.metadata.name, object.metadata.namespace);
    if (reviewed(live).uid !== reviewed(object).uid
      || at(live, "metadata", "labels", "app.kubernetes.io/managed-by") !== "Helm"
      || at(live, "metadata", "annotations", "meta.helm.sh/release-name") !== request.release
      || at(live, "metadata", "annotations", "meta.helm.sh/release-namespace") !== request.namespace) throw new Error(fail);
    return live;
  };
  for (const object of frozen) {
    const live = await resource(object);
    if (at(live, "metadata", "annotations", FENCE) !== marker || reviewed(live).resourceVersion === reviewed(object).resourceVersion) {
      await fence(run, object.kind, live, marker);
    }
  }
  for (const entry of await failedStorage()) {
    if (entry.object.immutable !== true || at(entry.object, "metadata", "annotations", FENCE) !== marker) {
      await fence(run, "secret", entry.object, marker, true);
    }
  }
  for (const object of frozen) {
    const live = await resource(object);
    if (reviewed(live).resourceVersion === reviewed(object).resourceVersion || at(live, "metadata", "annotations", FENCE) !== marker) throw new Error(fail);
  }
  if ((await failedStorage()).some(entry => entry.object.immutable !== true || at(entry.object, "metadata", "annotations", FENCE) !== marker)
    || artifactDigest(publication.artifact) !== publication.digest) throw new Error(fail);
  receipt = await advanceRootPublicationBundle(run, receipt, journal, frozen);
  const next = structuredClone(receipt), failed = next.publication!;
  failed.recovery!.fenced = true;
  next.failedPublications = [...(next.failedPublications ?? []), structuredClone(failed)];
  next.publication = { phase: "preparing", attempt: failed.recovery!.attempt, requestDigest,
    context: request.context, release: request.release, namespace: request.namespace, storage: failed.recovery!.storage };
  if (failed.witnessWriter) {
    const original: ObjectMap = failed.witnessWriter.deployment;
    const live = await read(run, "Deployment", reviewed(original).name, original.metadata.namespace);
    if (failed.witnessWriter.restored || reviewed(live).uid !== reviewed(original).uid || at(live, "spec", "replicas") !== 0
      || ![original.spec, failed.witnessWriter.publishedSpec].some(spec => spec
        && canonical({ ...record(spec), replicas: 0 }) === canonical(live.spec))) throw new Error(fail);
    const deployment = structuredClone(live);
    record(deployment.spec).replicas = original.spec.replicas;
    next.publication.witnessWriter = { deployment };
  }
  await assertRetiredRootPublication(run, next);
  next.namespace = await read(run, "namespace", request.namespace);
  await journal.save(next);
  return next;
}

/** Publishes an immutable server-rendered Core snapshot, not a second runtime or
 * an independently rendered Helm invocation. Every existing API object carries
 * UID/resourceVersion preconditions. Release Secrets become immutable after normal
 * Helm exit: Helm storage updates can omit resourceVersion, so RV fencing alone
 * cannot stop them. Unknown outcomes leave both journal and namespace owned. */
export async function publishPrivateCore(execute: SchemaExecute, request: PrivateCoreRequest): Promise<void> {
  const originalExecute = execute;
  execute = async (file, args, options) => {
    try {
      return await originalExecute(file, args, file === "helm"
        ? { ...options, env: { ...options.env, HELM_DRIVER: "secret" } } : options);
    } catch (error) {
      if (file === "kubectl") throw privateCommandFailure(error, args);
      if (file === "helm" && args[0] === "list" && args.includes("--all")
        && error instanceof Error && "exitCode" in error && error.exitCode === 1
        && "stderr" in error && typeof error.stderr === "string"
        && error.stderr.trim() === "Error: unknown flag: --all") {
        // Preserve only the public syntax classification needed by Helm 4 fallback.
        throw Object.assign(new Error("Error: unknown flag: --all"), { exitCode: 1, stderr: "Error: unknown flag: --all" });
      }
      throw new Error("Protected Core Helm operation failed; retain the private receipt and snapshot. Command output is withheld because it may contain credentials");
    }
  };
  const run: Execute = async (args, input) => (await execute("kubectl", args, { stdio: "pipe", ...(input ? { input } : {}) })).stdout;
  await withRootMigrationJournal(request.receipt, false, async (journal, original) => {
    let receipt = original!;
    const requestDigest = digest({ context: request.context, release: request.release, namespace: request.namespace,
      chart: resolve(request.chart), values: request.values.map(value => resolve(value)), set: request.set });
    if ([request.recoverFailedPublication, request.replacePreparingRequest, request.retireHistoricalMonitoringHooks].filter(Boolean).length > 1) {
      throw new Error("Choose one recovery operation: failed-publication recovery, preparing-request replacement, or historical monitoring hook retirement");
    }
    if (receipt.publication?.phase === "publishing"
      || (receipt.publication?.phase === "succeeded" && (request.recoverFailedPublication || receipt.publication.recovery))) {
      if (!request.recoverFailedPublication) throw new Error("Publication outcome is unknown; do not retry, rotate, finalize or clear ownership");
      const version = (await execute("helm", ["version", "--template", "{{.Version}}"], { stdio: "pipe" })).stdout.trim();
      if (!/^v4\.1\.3(?:\+[0-9A-Za-z.-]+)?$/.test(version)) throw new Error("Protected recovery requires inspected Helm 4.1.3 SSA behavior");
      receipt = await recoverFailedPublication(run, request, receipt, journal, requestDigest);
    } else if (request.recoverFailedPublication && !receipt.failedPublications?.some(value => value.recovery?.attempt === receipt.publication?.attempt)) {
      throw new Error("Failed-publication recovery requires its original publishing receipt or its journaled successor");
    }
    if (request.replacePreparingRequest) {
      const publication = receipt.publication;
      // Fail closed for all snapshot, adoption and launch outcomes, including
      // future fields. Replacement changes only the request, never ownership.
      const preparingFields = ["phase", "attempt", "context", "release", "namespace", "requestDigest", "storage", "witnessWriter"];
      const writer = publication?.witnessWriter;
      if (receipt.phase !== "rotation-required" || publication?.phase !== "preparing"
        || publication.requestDigest === requestDigest || !publication.storage?.length
        || Object.keys(publication).some(field => !preparingFields.includes(field))
        || (Object.hasOwn(publication, "witnessWriter") && (!writer
          || Object.keys(writer).some(field => !["deployment", "evidenceFence"].includes(field))
          || !writer.evidenceFence || canonical(Object.keys(writer.evidenceFence).sort()) !== canonical(["resourceVersion", "uid"])))
        || publication.context !== request.context || publication.release !== request.release || publication.namespace !== request.namespace
        || at(receipt.namespace, "metadata", "annotations", CORE_PUBLICATION) !== canonical({ version: 1,
          attempt: publication.attempt, planDigest: receipt.planDigest, context: publication.context, phase: "preparing" })) {
        throw new Error("Request replacement requires an owned rotation-required preparing publication with no snapshot or launch outcomes and the same context, release and namespace");
      }
    } else if (receipt.publication && receipt.publication.requestDigest !== requestDigest) {
      throw new Error("Resume requires the original Core publication request");
    }
    if (receipt.publication?.phase === "verified") {
      if (receipt.publication.witnessWriter && !receipt.publication.witnessWriter.restored) {
        if (!receipt.publication.artifact || artifactDigest(receipt.publication.artifact) !== receipt.publication.digest) throw new Error(fail);
        await assertRetiredRootPublication(run, receipt, false);
        await restorePublicationWitness(run, receipt, journal,
          schemaDocuments(readFileSync(join(receipt.publication.artifact, "files/publication.yaml"), "utf8")));
      }
      return;
    }
    if (request.retireHistoricalMonitoringHooks && !receipt.publication) {
      await retireHistoricalMonitoringHooks(run, request, receipt, journal);
    } else if (receipt.historicalHookRetirement?.phase === "prepared") {
      throw new Error("Resume historical monitoring hook retirement before Core publication");
    }
    const before = await storage(run, request, receipt);
    // Helm's CSA-to-SSA conversion updates original managedFields separately,
    // invalidating the frozen target RV. Do not begin any publisher mutation.
    if (storageBody(before.at(-1)!.object).release.apply_method !== "ssa") {
      throw new Error("Protected Core requires an explicit SSA predecessor release (apply_method=ssa); CSA, missing and unknown apply methods require a separately supported migration");
    }
    if (!receipt.publication || ["preparing", "prepared"].includes(receipt.publication.phase)) {
      const version = (await execute("helm", ["version", "--template", "{{.Version}}"], { stdio: "pipe" })).stdout.trim();
      if (!/^v4\.1\.3(?:\+[0-9A-Za-z.-]+)?$/.test(version)) throw new Error("Protected Core publication requires inspected Helm 4.1.3 SSA behavior");
      if (receipt.publication?.storage && canonical(receipt.publication.storage) !== canonical(savedStorage(before))) throw new Error(fail);
    }
    if (request.replacePreparingRequest) {
      await assertRetiredRootPublication(run, receipt);
      const publication = receipt.publication!, writer = publication.witnessWriter;
      if (writer) {
        // A fenced, stopped writer is preparation state, not a launch outcome.
        // Keep its original replica intent and fence; never pause or fence again here.
        await assertPublicationWitnessStopped(run, receipt);
        const evidence = await read(run, "ConfigMap", "kars-datapath-witness", publication.namespace);
        const writerNamespace = at(writer.deployment, "metadata", "namespace");
        if (typeof writerNamespace !== "string" || !writerNamespace) throw new Error(fail);
        const liveWriter = await read(run, "Deployment", reviewed(writer.deployment).name, writerNamespace);
        for (const object of [evidence, liveWriter]) {
          if (at(object, "metadata", "labels", "kars.azure.com/witness-addon") !== "true"
            || at(object, "metadata", "labels", "app.kubernetes.io/managed-by") !== "Helm"
            || at(object, "metadata", "annotations", "meta.helm.sh/release-name") !== publication.release
            || at(object, "metadata", "annotations", "meta.helm.sh/release-namespace") !== publication.namespace) throw new Error(fail);
        }
        if (reviewed(evidence).uid !== writer.evidenceFence!.uid
          || reviewed(evidence).resourceVersion === writer.evidenceFence!.resourceVersion
          || at(evidence, "metadata", "annotations", FENCE) !== `${publication.attempt}:witness-paused`) throw new Error(fail);
      }
      receipt = structuredClone(receipt);
      receipt.publication!.requestDigest = requestDigest;
      await journal.save(receipt);
    }
    if (!receipt.publication) {
      receipt = structuredClone(receipt);
      receipt.publication = { phase: "preparing", attempt: randomBytes(32).toString("hex"), context: request.context,
        release: request.release, namespace: request.namespace, requestDigest, storage: savedStorage(before) };
      await journal.save(receipt);
    }
    if (receipt.publication.phase === "preparing") {
      receipt.publication.storage ??= savedStorage(before);
      receipt = await ownRootPublication(run, receipt, journal);
      const args = ["upgrade", request.release, request.chart, "--namespace", request.namespace, "--reset-values", "--server-side=true",
        "--history-max", "0", "--timeout", "300s", "--wait=legacy",
        ...request.values.flatMap(value => ["-f", value]), ...request.set.flatMap(value => ["--set", value]), "--set", "controller.replicas=0"];
      const rendered = await renderCoreSchemaChart(execute, args);
      const preview = record(JSON.parse((await execute("helm", [...args, "--dry-run=server", "-o", "json"], { stdio: "pipe" })).stdout));
      if (preview.name !== request.release || preview.namespace !== request.namespace || preview.version !== before.length + 1) {
        throw new Error("Protected Core requires an exact server upgrade preview");
      }
      const { staticHooks, monitoring } = materializeStaticHooks(preview, before, request.namespace);
      const previous = schemaDocuments(String(storageBody(before.at(-1)!.object).release.manifest));
      const documents = [...schemaDocuments(String(preview.manifest)), ...staticHooks];
      // Monitoring crds/ bootstrap objects are qualified separately, not adopted into Helm's release manifest.
      for (const crd of coreSchemaDocuments(rendered.documents).filter(value => value.kind === "CustomResourceDefinition")) {
        if (!documents.some(value => key(value, request.namespace) === key(crd, request.namespace))) documents.push(crd);
      }
      // Read-only schema qualification of the maintained chart. The snapshot has
      // live CAS identities, which are deliberately forbidden in schema writes.
      await prepareCoreHelmSchemas(execute, args, { checkOnly: true });
      await assertRetiredRootPublication(run, receipt);
      for (const hook of staticHooks) await adoptStaticPolicy(run, request, receipt, journal, hook);
      await stopPublicationWitness(run, receipt, journal, documents);
      await fencePublicationWitness(run, receipt, journal, documents);
      const prepared = await prepareDocuments(execute, run, request, receipt, documents, previous, staticHooks, monitoring,
        at(preview, "config", "monitoring", "kube-state-metrics", "resources"));
      await assertRetiredRootPublication(run, receipt);
      const artifact = snapshot(request.receipt, receipt.publication!.attempt, preview, prepared.documents);
      receipt.publication = { ...receipt.publication!, ...artifact, phase: "prepared", revision: before.length + 1,
        storage: savedStorage(before), resources: prepared.resources };
      await journal.save(receipt);
    }
    const publication = receipt.publication!;
    if (!publication.artifact || !publication.digest || !publication.storage || !publication.resources
      || artifactDigest(publication.artifact) !== publication.digest) throw new Error("The private publication snapshot is missing or changed");
    if (publication.phase === "prepared") {
      await assertRetiredRootPublication(run, receipt);
      await assertPublicationWitnessStopped(run, receipt);
      if (canonical(savedStorage(await storage(run, request, receipt))) !== canonical(publication.storage)) throw new Error(fail);
      for (const object of schemaDocuments(readFileSync(join(publication.artifact, "files/publication.yaml"), "utf8"))) {
        const live = await read(run, object.kind, object.metadata.name, object.metadata.namespace);
        if (reviewed(live).uid !== reviewed(object).uid || reviewed(live).resourceVersion !== reviewed(object).resourceVersion) {
          throw new Error("Prepared Core snapshot has stale identities or versions; it has not been launched");
        }
        if (at(live, "metadata", "labels", "app.kubernetes.io/managed-by") !== "Helm"
          || at(live, "metadata", "annotations", "meta.helm.sh/release-name") !== request.release
          || at(live, "metadata", "annotations", "meta.helm.sh/release-namespace") !== request.namespace) {
          throw new Error("Prepared Core snapshot lacks live Helm ownership; it has not been launched");
        }
      }
      if (artifactDigest(publication.artifact) !== publication.digest) throw new Error(fail);
      receipt = await commitRootPublication(receipt, journal, async publishing => {
        Object.assign(publishing, await claimRootPublicationLaunch(run, publishing));
        if (artifactDigest(publication.artifact!) !== publication.digest) throw new Error(fail);
        const documents = schemaDocuments(readFileSync(join(publication.artifact!, "files/publication.yaml"), "utf8"));
        const namespace = documents.find(value => value.kind === "Namespace" && value.metadata.name === request.namespace);
        if (namespace) {
          namespace.metadata.resourceVersion = reviewed(publishing.namespace).resourceVersion;
          namespace.metadata.annotations[CORE_PUBLICATION] = at(publishing.namespace, "metadata", "annotations", CORE_PUBLICATION);
        }
        // Only the acknowledged ownership CAS changes this frozen projection.
        // Persist its new namespace version before any Helm process can start.
        Object.assign(publishing.publication!, snapshot(request.receipt, publication.attempt, {
          chart: { metadata: JSON.parse(readFileSync(join(publication.artifact!, "Chart.yaml"), "utf8")) },
          config: JSON.parse(readFileSync(join(publication.artifact!, "values.yaml"), "utf8")),
        }, documents));
        await journal.save(publishing);
        const args = ["upgrade", request.release, publishing.publication!.artifact!, "--namespace", request.namespace,
          "--reset-values", "--server-side=true", "--history-max", "0", "--timeout", "300s", "--wait=hookOnly"];
        try { await execute("helm", args, { stdio: "pipe" }); }
        catch { throw new Error("Protected Helm publication did not complete normally; outcome is unknown. Preserve receipt, history and namespace ownership"); }
        await assertPublicationWitnessStopped(run, publishing);
        await verifyStorage(run, request, publishing);
        await verifyResources(run, publishing);
        // Normal exit prevents new requests. Frozen SSA requests carry versions;
        // release UPDATEs may not. Immutable Secrets reject delayed data changes
        // and attempts to unset immutability, including unconditional Helm UPDATEs.
        // No failure/cancellation recovery is inferred.
        for (const resource of publication.resources!) {
          await fence(run, resource.kind, await read(run, resource.kind, resource.name, resource.namespace), publication.attempt);
        }
        for (const entry of await verifyStorage(run, request, publishing)) await fence(run, "secret", entry.object, publication.attempt, true);
        await verifyResources(run, publishing);
        if ((await verifyStorage(run, request, publishing)).some(entry => entry.object.immutable !== true)) throw new Error(fail);
      });
    }
    if (receipt.publication!.phase !== "succeeded") throw new Error(fail);
    if ((await verifyStorage(run, request, receipt)).some(entry => entry.object.immutable !== true)) throw new Error(fail);
    await verifyResources(run, receipt);
    // Wait after durable, fenced Helm completion so readiness retries never relaunch it.
    // Only the reviewed retired root is deferred to qualified activation.
    await waitForPublishedResources(run, receipt,
      schemaDocuments(readFileSync(join(receipt.publication!.artifact!, "files/publication.yaml"), "utf8")));
    await verifyResources(run, receipt);
    receipt = await advanceRootPublicationBundle(run, receipt, journal,
      schemaDocuments(readFileSync(join(receipt.publication!.artifact!, "files/publication.yaml"), "utf8")));
    await verifyResources(run, receipt);
    receipt = await releaseRootPublication(run, receipt, journal);
    await restorePublicationWitness(run, receipt, journal,
      schemaDocuments(readFileSync(join(receipt.publication!.artifact!, "files/publication.yaml"), "utf8")));
  });
}
