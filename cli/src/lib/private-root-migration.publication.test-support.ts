// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { expect } from "vitest";
import { randomUUID } from "node:crypto";
import { chmodSync, mkdirSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { join } from "node:path";
import { gunzipSync, gzipSync } from "node:zlib";
import { publishPrivateCore } from "./private-core-publication.js";
import { historicalHooks } from "./private-core-history.test-support.js";
import { HISTORICAL_HOOK_RETIREMENT } from "./private-core-history.js";
import { withRootMigrationJournal } from "./private-root-migration-journal.js";
import { crd, schemaFixture } from "./schema-stage.test-support.js";
import { schemaDocuments, type SchemaExecute } from "./schema-documents.js";
import { CORE_PUBLICATION, CORE_PUBLICATION_FENCE } from "./private-activation.js";
import { canonical } from "./private-activation.js";
import { rootPod } from "./private-activation-fixtures.js";
import { previewPrivateRootMigration } from "./private-root-migration-review.js";
import { prepareRootMigration, retireRootMigration, type RootMigrationReceipt } from "./private-root-migration.js";
import { setup, ORIGINAL, TARGET } from "./private-root-migration.test-support.js";

export async function publicationFixture(policyDocument?: any, extraDocuments: any[] = [], runnerOnly = false, companionReplicas?: number, fullBundle = false, foreignReplicas?: number) {
  const f = await setup(true, root => {
    root.spec.progressDeadlineSeconds = 600;
    if (runnerOnly) root.spec.template.spec.containers[0].env.push({ name: "KARS_CONFORMANCE_RUNNER_IMAGE", value: ORIGINAL });
  }, companionReplicas, foreignReplicas), schema = schemaFixture([crd()]);
  if (runnerOnly) {
    const container = f.rendered.spec.template.spec.containers[0];
    container.image = ORIGINAL;
    container.env.find((entry: any) => entry.name === "KARS_CONFORMANCE_RUNNER_IMAGE").value = TARGET;
  }
  schema.owner.namespace = "core";
  const definition = schema.install(crd());
  const policy: any = { apiVersion: "kars.azure.com/v1alpha1", kind: "ToolPolicy", metadata: {
    name: "kars-default", namespace: "core", uid: "tool-policy", resourceVersion: "1", labels: { "app.kubernetes.io/managed-by": "Helm" },
    annotations: { "helm.sh/hook": "post-install,post-upgrade", "helm.sh/hook-weight": "-5" } }, spec: { allowed: "retained-policy" } };
  if (policyDocument) Object.assign(policy, structuredClone(policyDocument), {
    metadata: { ...policyDocument.metadata, uid: "tool-policy", resourceVersion: "1" },
  });
  const objects: any[] = [f.root, f.secret, f.ca, definition, f.namespace("core"), policy,
    ...(fullBundle ? [...f.objects.values()].filter(value => ["ValidatingAdmissionPolicy", "ValidatingAdmissionPolicyBinding"].includes(value.kind)) : []),
    ...extraDocuments.map((object, index) => ({ ...structuredClone(object), metadata: {
      ...structuredClone(object.metadata), uid: `extra-${index}`, resourceVersion: "1", generation: 1,
    }, ...(object.kind === "ValidatingAdmissionPolicy" ? { status: { observedGeneration: 1, typeChecking: { expressionWarnings: [] } } } : {}) }))];
  f.namespace("core").kind = "Namespace";
  for (const object of objects) {
    object.apiVersion ??= "v1";
    object.kind ??= object === f.secret ? "Secret" : "Deployment";
    object.metadata.labels = { ...object.metadata.labels, "app.kubernetes.io/managed-by": "Helm" };
    if (object !== policy) object.metadata.annotations = { ...object.metadata.annotations,
      "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "core" };
  }
  const review = await previewPrivateRootMigration(f.execute, "core", "kars", [f.rendered]);
  const prepared = await prepareRootMigration(f.execute, review, [f.rendered], f.journal);
  const retired = await retireRootMigration(f.execute, prepared, f.journal);
  f.rotate(2);
  const directory = `.core-publication-test-${randomUUID()}`;
  mkdirSync(directory, { mode: 0o700 });
  const file = join(directory, "receipt.jsonl");
  await withRootMigrationJournal(file, true, async journal => journal.save(retired));
  const request = { receipt: file, context: "kind-owned", release: "kars", namespace: "core", chart: "charts/kars", values: [], set: [] };
  const documents = () => objects.map(object => {
    const copy = structuredClone(object);
    delete copy.status;
    for (const field of ["uid", "resourceVersion", "generation", "managedFields"]) delete copy.metadata[field];
    return copy;
  });
  const manifestObjects = (items: any[]) => items.map(object =>
    `${object.metadata.labels?.["app.kubernetes.io/part-of"] === "monitoring"
      ? `# Source: kars/charts/monitoring/templates/${object.kind}.yaml\n` : ""}${JSON.stringify(object)}`).join("\n---\n");
  const manifest = () => manifestObjects(documents().filter(object => object.kind !== "ToolPolicy"));
  const policyHook = () => ({ name: "kars-default", manifest: JSON.stringify(documents().find(object => object.kind === "ToolPolicy")),
    last_run: { phase: "Succeeded" } });
  const encode = (release: any) => Buffer.from(gzipSync(Buffer.from(JSON.stringify(release))).toString("base64")).toString("base64");
  const original: any = { name: "kars", namespace: "core", version: 1, apply_method: "ssa",
    info: { status: "deployed" }, manifest: manifest(), hooks: [policyHook()] };
  const secret = (release: any) => ({ apiVersion: "v1", kind: "Secret", type: "helm.sh/release.v1",
    metadata: { name: `sh.helm.release.v1.kars.v${release.version}`, namespace: "core", uid: `release-${release.version}`,
      resourceVersion: "1", labels: { owner: "helm", name: "kars", version: String(release.version), status: release.info.status } },
    data: { release: encode(release) } });
  const history: any[] = [secret(original)];
  const controls = { launchFailure: false, failedPublication: false, preflightFailure: false, afterSuccessFailure: false, lostReleaseAck: false,
    fenceFailure: false, immutableAckFailure: false, claimAckFailure: false, preparedInterruption: false,
    hooks: false, changedTls: false, failedHistory: false, omittedNamespace: false, launches: 0, fences: 0,
    adoptionBeforePatchFailure: false, adoptionAckFailure: false, adoptionAfterSaveFailure: false, adoptionBadAck: false,
    realHelmRender: false, manifestMutation: "", liveOwnershipChecks: 0, adoptions: 0, writes: 0,
    monitoringHooks: [] as any[], policyHook: undefined as Record<string, any> | undefined,
    config: {} as Record<string, any>, previewMutate: undefined as ((items: any[]) => void) | undefined,
    pendingRootReplicaSet: false, readinessFailure: "", readinessCalls: [] as string[][] };
  const current = async () => {
    let receipt: RootMigrationReceipt | undefined;
    await withRootMigrationJournal(file, false, async (_journal, value) => { receipt = value; });
    return receipt!;
  };
  const savedPublication = () => JSON.parse(readFileSync(file, "utf8").trim().split("\n").at(-1)!).receipt.publication;
  const phase = () => savedPublication()?.phase;
  const execute: SchemaExecute = async (tool, args, settings) => {
    if (tool === "kubectl" && ["patch", "create", "replace", "delete"].includes(args[0]!)) controls.writes++;
    if (controls.adoptionAfterSaveFailure && savedPublication()?.adoption?.appliedResourceVersion
      && tool === "kubectl" && args[0] === "apply") {
      controls.adoptionAfterSaveFailure = false;
      throw new Error("interrupted after adoption journal acknowledgement");
    }
    if (controls.preparedInterruption && phase() === "prepared") {
      controls.preparedInterruption = false;
      throw new Error("interrupted before launch");
    }
    if (tool === "helm") {
      expect(settings.env?.HELM_DRIVER).toBe("secret");
      if (args[0] === "version") return { stdout: "v4.1.3" };
      if (args[0] === "list") return { stdout: JSON.stringify([{ name: "kars", namespace: "core" }]) };
      if (args[0] === "history") return { stdout: JSON.stringify([{ revision: 1, status: "deployed" }]) };
      if (args[0] === "get") return { stdout: original.manifest };
      if (args[0] === "template") {
        if (controls.preflightFailure && args.includes("--dry-run=server")) {
          controls.preflightFailure = false;
          throw new Error("server preflight interrupted");
        }
        return { stdout: manifest() };
      }
      if (args[0] === "upgrade" && args.includes("--dry-run=server")) {
        const desired = documents().filter(object => object.kind !== "ToolPolicy");
        if (controls.changedTls) desired.find(value => value.kind === "Secret")!.data["tls.crt"] = "changed";
        if (controls.omittedNamespace) delete desired.find(value => value.kind === "Deployment")!.metadata.namespace;
        controls.previewMutate?.(desired);
        return { stdout: JSON.stringify({ ...original, version: history.length + 1, manifest: manifestObjects(desired),
          hooks: controls.hooks ? [{ name: "writer" }] : [controls.policyHook ?? policyHook(), ...controls.monitoringHooks],
          chart: { metadata: { name: "kars", version: "0.1.0" } }, config: controls.config }) };
      }
      if (args[0] === "upgrade") {
        controls.launches++;
        expect(phase()).toBe("publishing");
        expect(f.namespace("core").metadata.annotations[CORE_PUBLICATION]).toBeDefined();
        expect(args).toContain("--server-side=true");
        expect(args).toContain("--wait=hookOnly");
        expect(args.slice(args.indexOf("--history-max"), args.indexOf("--history-max") + 2)).toEqual(["--history-max", "0"]);
        if (controls.launchFailure) throw new Error("Helm resource writer outlived cancellation");
        const frozen = readFileSync(join(args[2]!, "files/publication.yaml"), "utf8");
        const resourceKey = (value: any) => canonical([value.apiVersion, value.kind, value.metadata.namespace ?? "core", value.metadata.name]);
        const existing = new Set(schemaDocuments(original.manifest).map(resourceKey));
        // Helm existingResourceConflict inspects LIVE ownership for resources absent
        // from the predecessor manifest, before it can apply desired annotations.
        for (const object of schemaDocuments(frozen).filter(value => !existing.has(resourceKey(value)))) {
          controls.liveOwnershipChecks++;
          const live = objects.find(value => resourceKey(value) === resourceKey(object))!;
          if (live.metadata.labels?.["app.kubernetes.io/managed-by"] !== "Helm"
            || live.metadata.annotations?.["meta.helm.sh/release-name"] !== "kars"
            || live.metadata.annotations?.["meta.helm.sh/release-namespace"] !== "core") {
            throw new Error("existingResourceConflict: live resource lacks Helm ownership");
          }
        }
        if (original.apply_method !== "ssa" && f.root.metadata.managedFields?.some((field: any) => field.manager === "helm" && field.operation === "Update")) {
          // Helm's CSA field-manager upgrade modifies the original separately from
          // its pinned target. The protected publisher must reject before this.
          f.root.metadata.resourceVersion = String(Number(f.root.metadata.resourceVersion) + 1);
        }
        let publishedManifest = frozen;
        if (controls.realHelmRender) {
          publishedManifest = execFileSync("helm", ["template", "kars", args[2]!, "--namespace", "core"],
            { encoding: "utf8", timeout: 10_000, stdio: ["ignore", "pipe", "pipe"] });
          expect(canonical(schemaDocuments(publishedManifest))).not.toBe(canonical(schemaDocuments(frozen)));
          const sortedKinds = schemaDocuments(publishedManifest).map(value => value.kind);
          expect(sortedKinds.indexOf("ToolPolicy")).toBeLessThan(sortedKinds.indexOf("ValidatingAdmissionPolicy"));
          expect(sortedKinds.indexOf("CustomResourceDefinition")).toBeLessThan(sortedKinds.indexOf("Deployment"));
        }
        const released = schemaDocuments(publishedManifest);
        if (controls.manifestMutation === "duplicate") released.push(structuredClone(released[0]!));
        if (controls.manifestMutation === "missing") released.pop();
        if (controls.manifestMutation === "resource-version") released[0]!.metadata.resourceVersion = "stale-version";
        if (controls.manifestMutation === "body") released[0]!.metadata.labels.unreviewed = "true";
        if (controls.manifestMutation === "namespace") released.find(value => value.kind === "ToolPolicy")!.metadata.namespace = "other";
        for (const object of schemaDocuments(frozen)) {
          if (controls.failedPublication && object.kind === "ConfigMap") continue;
          const live = objects.find(value => value.kind === object.kind && value.metadata.name === object.metadata.name)!;
          expect(object.metadata.uid).toBe(live.metadata.uid);
          expect(object.metadata.resourceVersion).toBe(live.metadata.resourceVersion);
          if (live === f.root) object.spec.progressDeadlineSeconds ??= 600;
          Object.assign(live, object, { metadata: { ...live.metadata, ...object.metadata, resourceVersion: String(Number(live.metadata.resourceVersion) + 1) } });
        }
        const predecessor = history.at(-1)!;
        if (!predecessor.immutable && !controls.failedPublication) {
          const previous = JSON.parse(gunzipSync(Buffer.from(Buffer.from(predecessor.data.release, "base64").toString(), "base64")).toString());
          previous.info.status = "superseded";
          predecessor.metadata.labels.status = "superseded";
          predecessor.data.release = encode(previous);
          predecessor.metadata.resourceVersion = String(Number(predecessor.metadata.resourceVersion) + 1);
        }
        history.push(secret({ ...original, version: history.length + 1, apply_method: "ssa", info: { status: controls.failedPublication ? "failed" : "deployed" },
          config: {}, chart: {
            metadata: JSON.parse(readFileSync(join(args[2]!, "Chart.yaml"), "utf8")),
            values: JSON.parse(readFileSync(join(args[2]!, "values.yaml"), "utf8")),
            templates: [{ name: "templates/publication.yaml", data: readFileSync(join(args[2]!, "templates/publication.yaml")).toString("base64") }],
            files: [{ name: "files/publication.yaml", data: Buffer.from(frozen).toString("base64") }],
          }, manifest: released.map(value => JSON.stringify(value)).join("\n---\n"), hooks: [] }));
        if (controls.pendingRootReplicaSet) {
          expect(f.root.spec.replicas).toBe(0);
          f.root.status.observedGeneration = f.root.metadata.generation - 1;
          f.root.status.conditions = [{ type: "Progressing", status: "False", reason: "ReplicaSetCreateError",
            message: "Private capability consumption requires qualified actor authority; an epoch alone grants none" }];
          if (args.includes("--wait=legacy")) throw new Error("context deadline exceeded");
        }
        if (controls.failedPublication) throw new Error("Helm failed after partially applying the frozen snapshot");
        return { stdout: "" };
      }
      throw new Error(`Unexpected Helm command ${args.join(" ")}`);
    }
    if (controls.afterSuccessFailure && phase() === "succeeded") {
      controls.afterSuccessFailure = false;
      throw new Error("interrupted after durable completion");
    }
    if (["rollout", "wait"].includes(args[0]!) && phase() === "succeeded") {
      controls.readinessCalls.push([...args]);
      expect(history.every(entry => entry.immutable === true)).toBe(true);
      if (controls.readinessFailure && args.includes(controls.readinessFailure)) throw new Error("resource readiness timeout");
      return { stdout: "" };
    }
    if (args[0] === "get" && args[1] === "secrets") {
      if (controls.failedHistory) {
        history[0]!.metadata.labels.status = "failed";
        history[0]!.data.release = encode({ ...original, info: { status: "failed" } });
      }
      return { stdout: JSON.stringify({ metadata: {}, items: history }) };
    }
    if (args[0] === "get" && args.includes("--raw")) return schema.execute(tool, args, settings);
    const live = [...objects, ...history].find(value => value.kind.toLowerCase() === args[1]?.toLowerCase()
      && value.metadata.name === args[2]);
    if (args[0] === "get" && live && args.includes("json")) return { stdout: JSON.stringify(live) };
    if (args[0] === "get" && args[1] === "customresourcedefinition") return schema.execute(tool, args, settings);
    if (args[0] === "apply") {
      expect(args).toContain("--dry-run=server");
      const desired = JSON.parse(settings.input!);
      const actual = objects.find(value => value.kind === desired.kind && value.metadata.name === desired.metadata.name)!;
      if (desired.metadata.namespace && args.includes("--namespace")) {
        expect(args[args.indexOf("--namespace") + 1]).toBe(desired.metadata.namespace);
      }
      if (actual === f.root) desired.spec.progressDeadlineSeconds ??= 600;
      return { stdout: JSON.stringify({ ...actual, ...desired, metadata: { ...actual.metadata, ...desired.metadata } }) };
    }
    if (args[0] === "patch" && args.includes("--type=json")) {
      const patch = JSON.parse(settings.input!);
      if (args[1] === "ToolPolicy" && patch.some((operation: any) => operation.path === "/metadata/annotations"
        && operation.value?.["meta.helm.sh/release-name"] === "kars" && !operation.value?.[CORE_PUBLICATION_FENCE])) {
        const publication = JSON.parse(readFileSync(file, "utf8").trim().split("\n").at(-1)!).receipt.publication;
        expect(publication.phase).toBe("preparing");
        expect(publication.adoption.uid).toBe(live.metadata.uid);
        expect(publication.adoption.resourceVersion).toBe(live.metadata.resourceVersion);
        expect(publication.artifact).toBeUndefined();
        if (controls.adoptionBeforePatchFailure) {
          controls.adoptionBeforePatchFailure = false;
          throw new Error("interrupted after journal intent, before adoption CAS");
        }
        expect(patch[0]).toEqual({ op: "test", path: "/metadata/uid", value: live.metadata.uid });
        expect(patch[1]).toEqual({ op: "test", path: "/metadata/resourceVersion", value: live.metadata.resourceVersion });
        expect(patch[2]).toEqual({ op: "test", path: "/metadata/annotations", value: live.metadata.annotations });
        live.metadata.annotations = patch[3].value;
        live.metadata.resourceVersion = String(Number(live.metadata.resourceVersion) + 1);
        controls.adoptions++;
        if (controls.adoptionAckFailure) {
          controls.adoptionAckFailure = false;
          throw new Error("adoption acknowledgement lost");
        }
        if (controls.adoptionBadAck) {
          controls.adoptionBadAck = false;
          const result = structuredClone(live);
          delete result.metadata.annotations["meta.helm.sh/release-name"];
          return { stdout: JSON.stringify(result) };
        }
        return { stdout: JSON.stringify(live) };
      }
      if (patch.some((operation: any) => operation.op === "add" && operation.value?.[CORE_PUBLICATION_FENCE])) {
        controls.fences++;
        if (controls.fenceFailure) throw new Error("fence acknowledgement lost");
        expect(patch[0].value).toBe(live.metadata.uid);
        expect(patch[1].value).toBe(live.metadata.resourceVersion);
        live.metadata.annotations = patch[2].value;
        if (patch.some((operation: any) => operation.path === "/immutable" && operation.value === true)) live.immutable = true;
        live.metadata.resourceVersion = String(Number(live.metadata.resourceVersion) + 1);
        if (controls.immutableAckFailure && live.type === "helm.sh/release.v1") {
          return { stdout: JSON.stringify({ ...live, immutable: false }) };
        }
        return { stdout: JSON.stringify(live) };
      }
      if (controls.claimAckFailure && patch.some((operation: any) => operation.path === "/metadata/annotations"
        && operation.op === "replace" && JSON.parse(operation.value?.[CORE_PUBLICATION] ?? "{}").phase === "publishing")) {
        await f.execute([...args], settings.input);
        throw new Error("launch ownership acknowledgement lost");
      }
      if (controls.lostReleaseAck && patch.some((operation: any) => operation.path === "/metadata/annotations"
        && operation.op === "replace" && !operation.value[CORE_PUBLICATION])) {
        controls.lostReleaseAck = false;
        await f.execute([...args], settings.input);
        throw new Error("ownership release acknowledgement lost");
      }
    }
    return { stdout: await f.execute([...args], settings.input) };
  };
  const cleanup = () => {
    for (const item of readdirSync(directory, { withFileTypes: true })) if (item.isDirectory()) {
      for (const child of [".", "templates", "files"]) chmodSync(join(directory, item.name, child), 0o700);
    }
    rmSync(directory, { recursive: true, force: true });
  };
  return { ...f, request, execute, current, controls, history, retired, cleanup, policy, original, encode, objects,
    migrationExecute: f.execute, rootPod: () => rootPod(f), publish: () => publishPrivateCore(execute, request) };
}

export async function witnessPublicationFixture(replicas = 1, fullBundle = false) {
  const f = await publicationFixture(undefined, [
    { apiVersion: "v1", kind: "ConfigMap", metadata: { name: "kars-datapath-witness", namespace: "core",
      labels: { "kars.azure.com/witness-addon": "true" } }, data: { "witness.json": "retained runtime evidence" } },
    { apiVersion: "apps/v1", kind: "Deployment", metadata: { name: "aggregator", namespace: "custom-witness",
      labels: { "kars.azure.com/witness-addon": "true" } }, spec: { replicas,
      selector: { matchLabels: { "app.kubernetes.io/component": "aggregator" } },
      template: { metadata: { labels: { "app.kubernetes.io/component": "aggregator" } },
        spec: { containers: [{ name: "aggregator", image: "witness:original" }] } } },
    },
  ], false, undefined, fullBundle);
  const writer = f.objects.find(value => value.metadata.name === "aggregator")!;
  const evidence = f.objects.find(value => value.metadata.name === "kars-datapath-witness")!;
  writer.status = { observedGeneration: writer.metadata.generation, replicas };
  const staleWitnessVersion = evidence.metadata.resourceVersion;
  const state = { restoreAckLost: false, rescaleBeforeLaunch: false, pauseAckLost: false, evidenceAckLost: false,
    afterLaunch: undefined as (() => void) | undefined, writes: [] as number[] };
  f.controls.previewMutate = documents => {
    const desired = documents.find(value => value.metadata.name === "aggregator")!;
    desired.spec.replicas = 1;
    desired.spec.template.spec.containers[0].image = "witness:updated";
  };
  const execute: SchemaExecute = async (tool, args, settings) => {
    if (tool === "kubectl" && args[0] === "get" && args.includes("custom-witness")
      && ["pods", "replicasets", "horizontalpodautoscalers"].includes(args[1]!)) return { stdout: JSON.stringify({ metadata: {}, items: [] }) };
    if (tool === "kubectl" && args[0] === "patch" && args[1] === "Deployment" && args[2] === "aggregator") {
      const patch = JSON.parse(settings.input!);
      const scale = patch.find((op: any) => op.path === "/spec/replicas");
      if (scale) {
        expect(patch.slice(0, 3)).toEqual([
          { op: "test", path: "/metadata/uid", value: writer.metadata.uid },
          { op: "test", path: "/metadata/resourceVersion", value: writer.metadata.resourceVersion },
          { op: "test", path: "/spec", value: writer.spec },
        ]);
        const receipt = JSON.parse(readFileSync(f.request.receipt, "utf8").trim().split("\n").at(-1)!).receipt;
        expect(receipt.publication.phase).toBe(scale.value === 0 ? "preparing" : "verified");
        expect(receipt.publication.witnessWriter.deployment.spec.replicas).toBe(replicas);
        writer.spec.replicas = scale.value;
        writer.metadata.resourceVersion = String(Number(writer.metadata.resourceVersion) + 1);
        writer.metadata.generation++;
        writer.status = { observedGeneration: writer.metadata.generation, replicas: scale.value };
        state.writes.push(scale.value);
        if (state.restoreAckLost && scale.value === 1) { state.restoreAckLost = false; throw new Error("restore acknowledgement lost"); }
        if (state.pauseAckLost && scale.value === 0) { state.pauseAckLost = false; throw new Error("pause acknowledgement lost"); }
        return { stdout: JSON.stringify(writer) };
      }
    }
    if (tool === "kubectl" && args[0] === "get" && args[1]?.toLowerCase() === "deployment" && args[2] === "aggregator"
      && state.rescaleBeforeLaunch) {
      const receipt = JSON.parse(readFileSync(f.request.receipt, "utf8").trim().split("\n").at(-1)!).receipt;
      if (receipt.publication?.phase === "prepared") writer.spec.replicas = 1;
    }
    if (tool === "helm" && args[0] === "upgrade" && !args.includes("--dry-run=server")) {
      expect(writer.spec.replicas).toBe(0);
      expect(evidence.metadata.resourceVersion).not.toBe(staleWitnessVersion);
      expect(evidence.metadata.annotations[CORE_PUBLICATION_FENCE]).toBeDefined();
      expect(evidence.data["witness.json"]).toBe("retained runtime evidence");
    }
    if (tool === "kubectl" && args[0] === "patch" && args[2] === "kars-datapath-witness") {
      const annotations = JSON.parse(settings.input!).find((op: any) => op.path === "/metadata/annotations").value;
      expect(canonical(annotations)).not.toBe(canonical(evidence.metadata.annotations));
    }
    const result = await f.execute(tool, args, settings);
    if (tool === "helm" && args[0] === "upgrade" && !args.includes("--dry-run=server")) state.afterLaunch?.();
    if (state.evidenceAckLost && tool === "kubectl" && args[0] === "patch" && args[2] === "kars-datapath-witness") {
      state.evidenceAckLost = false;
      throw new Error("evidence fence acknowledgement lost");
    }
    return result;
  };
  return { ...f, execute, writer, evidence, state, publish: () => publishPrivateCore(execute, f.request) };
}

export async function failedPublicationFixture(fullBundle = false) {
  const f = await publicationFixture(undefined, [], false, undefined, fullBundle);
  f.controls.policyHook = structuredClone(f.original.hooks[0]);
  f.controls.failedPublication = true;
  await expect(f.publish()).rejects.toThrow("outcome is unknown");
  f.controls.failedPublication = false;
  const failed = (await f.current()).publication!;
  return { ...f, failed, recover: (execute = f.execute) => publishPrivateCore(execute, { ...f.request, recoverFailedPublication: true }) };
}

export async function monitoringPublicationFixture(extraDocuments: any[] = []) {
  const f = await publicationFixture(undefined, [{ apiVersion: "apps/v1", kind: "Deployment", metadata: {
    name: "custom-monitoring-operator", namespace: "observability", labels: { "app.kubernetes.io/part-of": "monitoring" },
  }, spec: { replicas: 1, template: { spec: { containers: [{ name: "operator", image: "operator:retained" }] } } } }, ...extraDocuments]);
  const definitions: Record<string, string> = { "serviceaccount.yaml": "ServiceAccount", "clusterrole.yaml": "ClusterRole",
    "clusterrolebinding.yaml": "ClusterRoleBinding", "role.yaml": "Role", "rolebinding.yaml": "RoleBinding",
    "job-createSecret.yaml": "Job", "job-patchWebhook.yaml": "Job" };
  f.controls.monitoringHooks = Object.entries(definitions).map(([file, kind]) => ({
    name: `custom-${file}`, kind, path: `kars/charts/monitoring/templates/prometheus-operator/admission-webhooks/job-patch/${file}`,
    events: ["pre-install", "pre-upgrade"], delete_policies: ["before-hook-creation", "hook-succeeded"],
    manifest: JSON.stringify({ apiVersion: kind === "Job" ? "batch/v1" : "v1", kind,
      metadata: { name: `custom-${file}`, namespace: "observability" } }),
  }));
  f.controls.config = { monitoring: { enabled: true, fullnameOverride: "custom-monitoring" } };
  f.original.config = structuredClone(f.controls.config);
  f.original.hooks.push(...f.controls.monitoringHooks.map(hook => ({ ...structuredClone(hook), last_run: { phase: "Succeeded" } })));
  f.history[0]!.data.release = f.encode(f.original);
  return f;
}

export async function monitoringCapacityFixture() {
  const before = { requests: { cpu: "25m", memory: "32Mi" }, limits: { cpu: "250m", memory: "128Mi" } };
  const resources = { requests: { cpu: "25m", memory: "64Mi" }, limits: { cpu: "500m", memory: "256Mi" } };
  const labels = { "app.kubernetes.io/name": "kube-state-metrics", "app.kubernetes.io/part-of": "monitoring" };
  const f = await monitoringPublicationFixture([{ apiVersion: "apps/v1", kind: "Deployment", metadata: {
    name: "custom-kube-state-metrics", namespace: "observability", labels,
  }, spec: { replicas: 1, selector: { matchLabels: { "app.kubernetes.io/name": "kube-state-metrics" } }, template: {
    metadata: { labels }, spec: { serviceAccountName: "metrics-account", containers: [{ name: "kube-state-metrics", image: "metrics:retained",
      resources: before, livenessProbe: { httpGet: { path: "/livez", port: 8080 } }, readinessProbe: { httpGet: { path: "/readyz", port: 8081 } },
    }] },
  } } }]);
  f.original.config.monitoring["kube-state-metrics"] = { resources: before };
  f.history[0].data.release = f.encode(f.original);
  f.controls.config.monitoring["kube-state-metrics"] = { resources };
  f.controls.previewMutate = items => {
    items.find(object => object.metadata.name === "custom-kube-state-metrics").spec.template.spec.containers[0].resources = structuredClone(resources);
  };
  return { ...f, resources, metrics: f.objects.find(object => object.metadata.name === "custom-kube-state-metrics")! };
}

export async function historicalPublicationFixture() {
  const f = await publicationFixture();
  const hooks = historicalHooks();
  hooks[7] = { ...f.original.hooks[0], kind: "ToolPolicy", path: "kars/templates/toolpolicy-default.yaml" };
  f.original.hooks = hooks;
  const failed = { ...structuredClone(f.original), info: { status: "failed" } };
  failed.hooks[6].last_run.phase = "";
  f.history[0].metadata.labels.status = "failed";
  f.history[0].data.release = f.encode(failed);
  f.original.version = 2;
  f.history.push({ ...structuredClone(f.history[0]), metadata: { ...structuredClone(f.history[0].metadata),
    name: "sh.helm.release.v1.kars.v2", uid: "release-2", labels: { owner: "helm", name: "kars", version: "2", status: "deployed" } },
    data: { release: f.encode(f.original) } });
  const state = { interrupt: "", live: "", freezes: 0 };
  const execute: SchemaExecute = async (tool, args, settings) => {
    if (tool === "kubectl" && args[0] === "get" && args.includes("--ignore-not-found") && args[2]?.startsWith("monitoring-admission")) {
      return { stdout: args[1] === state.live ? "{}" : "" };
    }
    if (tool === "kubectl" && args[0] === "get" && args[1] === "pods" && args.includes("monitoring")) {
      return { stdout: JSON.stringify({ metadata: {}, items: state.live === "pod" ? [{ spec: { serviceAccountName: "monitoring-admission" } }] : [] }) };
    }
    if (tool === "kubectl" && args[0] === "patch" && settings.input) {
      const patch = JSON.parse(settings.input), marker = patch.find((op: any) => op.value?.[HISTORICAL_HOOK_RETIREMENT]);
      if (marker && !marker.value[CORE_PUBLICATION_FENCE]) {
        const journal = JSON.parse(readFileSync(f.request.receipt, "utf8").trim().split("\n").at(-1)!).receipt;
        expect(journal.historicalHookRetirement.phase).toBe("prepared");
        expect(journal.historicalHookRetirement.proofs[0].failed.uid).toBe(f.history[0].metadata.uid);
        expect(journal.publication).toBeUndefined();
        if (state.interrupt === "before") { state.interrupt = ""; throw new Error("before historical freeze"); }
        const live = f.history[0];
        expect(patch.slice(0, 2)).toEqual([{ op: "test", path: "/metadata/uid", value: live.metadata.uid },
          { op: "test", path: "/metadata/resourceVersion", value: live.metadata.resourceVersion }]);
        live.metadata.annotations = marker.value;
        live.metadata.resourceVersion = String(Number(live.metadata.resourceVersion) + 1);
        live.immutable = true;
        state.freezes++;
        if (state.interrupt === "after") { state.interrupt = ""; throw new Error("historical freeze acknowledgement lost"); }
        return { stdout: JSON.stringify(live) };
      }
    }
    return f.execute(tool, args, settings);
  };
  return { ...f, state, execute, retireHooks: () => publishPrivateCore(execute, { ...f.request, retireHistoricalMonitoringHooks: true }),
    publish: () => publishPrivateCore(execute, f.request) };
}
