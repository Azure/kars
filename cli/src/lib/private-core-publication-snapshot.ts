// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { randomBytes } from "node:crypto";
import {
  constants, chmodSync, closeSync, fchmodSync, fstatSync, fsyncSync, lstatSync, mkdirSync, openSync,
  readFileSync, readdirSync, realpathSync, writeFileSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { gunzipSync } from "node:zlib";
import { parseAllDocuments } from "yaml";
import {
  at, canonical, CORE_PUBLICATION, CORE_PUBLICATION_FENCE as FENCE, digest, record,
} from "./private-activation.js";
import { schemaDocuments, type ObjectMap } from "./schema-documents.js";
import { type RootMigrationReceipt } from "./private-root-migration.js";
type Publication = NonNullable<RootMigrationReceipt["publication"]>;
export type Storage = NonNullable<Publication["storage"]>[number] & { object: ObjectMap; status: string };

const files = ["Chart.yaml", "values.yaml", "templates/publication.yaml", "files/publication.yaml"];

const publicationTemplate = '{{ .Files.Get "files/publication.yaml" }}\n';

export const fail = "Protected Core publication changed or lacks complete identity/version fencing; retain its receipt and Pending protection";

export function key(value: unknown, namespace: string): string {
  return canonical([at(value, "apiVersion"), at(value, "kind"), at(value, "metadata", "namespace") ?? namespace, at(value, "metadata", "name")]);
}

export function body(value: unknown): ObjectMap {
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

export function staticPolicyBody(value: ObjectMap): ObjectMap {
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

export function materializeStaticHooks(preview: ObjectMap, prior: Storage[], namespace: string): { staticHooks: ObjectMap[]; monitoring: string[] } {
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

export function storageBody(object: unknown): { release: ObjectMap; fingerprint: string } {
  const encoded = at(object, "data", "release");
  if (typeof encoded !== "string" || encoded.length > 2_097_152) throw new Error(fail);
  const binary = Buffer.from(Buffer.from(encoded, "base64").toString("utf8"), "base64");
  const raw = binary[0] === 0x1f && binary[1] === 0x8b ? gunzipSync(binary, { maxOutputLength: 16_777_216 }) : binary;
  const release = record(JSON.parse(raw.toString("utf8")));
  const copy = structuredClone(release);
  delete record(copy.info).status;
  return { release, fingerprint: digest(copy) };
}

function syncDirectory(path: string): void {
  const fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try { fsyncSync(fd); } finally { closeSync(fd); }
}

export function artifactDigest(path: string): string {
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

export function snapshot(receipt: string, attempt: string, preview: ObjectMap, documents: ObjectMap[]): { artifact: string; digest: string } {
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

export function unchangedBody(projected: ObjectMap, live: ObjectMap, receipt: RootMigrationReceipt, monitoringResources?: unknown): boolean {
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

export function manifestInventory(manifest: string): string {
  const documents = schemaDocuments(manifest), inventory = new Map<string, ObjectMap>();
  for (const object of documents) {
    const id = key(object, "");
    if (inventory.has(id)) throw new Error("Published Helm manifest differs from the owned snapshot: duplicate resource");
    inventory.set(id, object);
  }
  if (!inventory.size || inventory.size > 512) throw new Error("Published Helm manifest differs from the owned snapshot: incomplete inventory");
  return canonical([...inventory.entries()].sort(([left], [right]) => left.localeCompare(right)));
}
