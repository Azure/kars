// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { normalizedCrd, schemaDigest, SCHEMA_DIGEST, schemaOwnerFields, type ObjectMap, type SchemaExecute, type SchemaOwner } from "./schema-documents.js";

export function crd(kind = "KarsCredentialGrant", plural = "karscredentialgrants"): ObjectMap {
  return { apiVersion: "apiextensions.k8s.io/v1", kind: "CustomResourceDefinition",
    metadata: { name: `${plural}.kars.azure.com`, labels: { "app.kubernetes.io/name": "kars" } },
    spec: { group: "kars.azure.com", names: { kind, plural, singular: kind.toLowerCase() }, scope: "Namespaced",
      versions: [{ name: "v1alpha1", served: true, storage: true, schema: { openAPIV3Schema: {
        type: "object", required: ["spec"], properties: { metadata: { type: "object" }, spec: {
          type: "object", properties: { enabled: { type: "boolean", default: true }, workspaceUid: { type: "string" } },
        } },
      } } }] } };
}

export function admission(): ObjectMap {
  return { apiVersion: "admissionregistration.k8s.io/v1", kind: "ValidatingAdmissionPolicy",
    metadata: { name: "kars-credential-source-writes" }, spec: { failurePolicy: "Fail",
      paramKind: { apiVersion: "kars.azure.com/v1alpha1", kind: "KarsCredentialGrant" },
      validations: [{ expression: "params.spec.enabled" }] } };
}

export function schemaFixture(documents = [crd(), admission()]) {
  const owner: SchemaOwner = { ownership: "helm", release: "kars", namespace: "kars-system" };
  const objects = new Map<string, ObjectMap>();
  const requests: { file: string; args: readonly string[]; input?: string }[] = [];
  const writes: ObjectMap[] = [];
  const options = { established: true, published: true, resourceVisible: true, dangling: false,
    changedType: false, duplicate: false, link: "/openapi/v3/apis/kars.azure.com/v1alpha1?hash=current" };
  let time = 0;
  let revision = 1;
  let onSleep = () => {};
  let beforeWrite = (_object: ObjectMap) => {};
  let beforeRaw = (_path: string) => {};
  const install = (object: ObjectMap, metadataOwner: SchemaOwner = owner) => {
    const result = structuredClone(object);
    result.metadata = { ...result.metadata, uid: `${result.metadata.name}-uid`, resourceVersion: String(revision++),
      generation: 1, ...schemaOwnerFields(metadataOwner) };
    result.metadata.annotations[SCHEMA_DIGEST] = schemaDigest(normalizedCrd(result));
    result.status = { storedVersions: ["v1alpha1"], conditions: [{ type: "Established", status: "True" }] };
    objects.set(result.metadata.name, result);
    return result;
  };
  const execute: SchemaExecute = async (file, args, settings) => {
    requests.push({ file, args, input: settings.input });
    if (file === "helm") {
      if (args[0] === "template") return { stdout: documents.map(document => JSON.stringify(document)).join("\n---\n") };
      if (args[0] === "list") return { stdout: JSON.stringify([{ name: owner.release, namespace: owner.namespace }]) };
      if (args[0] === "get" && args[1] === "values") return { stdout: JSON.stringify({ preserved: "saved" }) };
      if (args[0] === "get" && args[1] === "manifest") return { stdout: documents.map(document => JSON.stringify(document)).join("\n---\n") };
      if (args[0] === "upgrade" || args[0] === "install") return { stdout: "" };
    }
    if (file !== "kubectl") throw new Error(`Unexpected fixture tool ${file}`);
    if (args[0] === "get" && args.includes("--raw")) {
      const path = args[args.indexOf("--raw") + 1];
      beforeRaw(path);
      if (path === "/openapi/v3") return { stdout: JSON.stringify({ paths: options.published
        ? { "apis/kars.azure.com/v1alpha1": { serverRelativeURL: options.link } } : {} }) };
      const crds = [...objects.values()].filter(object => object.kind === "CustomResourceDefinition");
      if (path === "/apis/kars.azure.com/v1alpha1") return { stdout: JSON.stringify({
        groupVersion: "kars.azure.com/v1alpha1", resources: options.resourceVisible ? crds.map(object => ({
          name: object.spec.names.plural, kind: object.spec.names.kind, namespaced: object.spec.scope === "Namespaced",
        })) : [],
      }) };
      if (path.startsWith("/openapi/v3/apis/kars.azure.com/v1alpha1?")) {
        const schemas: ObjectMap = { ObjectMeta: { type: "object", properties: { uid: { type: "string" } } } };
        for (const object of crds) {
          const schema = structuredClone(object.spec.versions[0].schema.openAPIV3Schema);
          schema["x-kubernetes-group-version-kind"] = [{ group: object.spec.group, version: "v1alpha1", kind: object.spec.names.kind }];
          schema.properties ??= {};
          schema.properties.metadata = { allOf: [{ $ref: "#/components/schemas/ObjectMeta" }], description: "API metadata" };
          if (options.changedType) schema.properties.spec = { type: "string" };
          schemas[object.spec.names.kind] = schema;
          if (options.duplicate) schemas[`${object.spec.names.kind}Duplicate`] = structuredClone(schema);
        }
        if (options.dangling) delete schemas.ObjectMeta;
        return { stdout: JSON.stringify({ components: { schemas } }) };
      }
      throw new Error(`Unexpected raw route ${path}`);
    }
    if (args[0] === "get") {
      const current = objects.get(`${args[1]}/${args[2]}`) ?? objects.get(args[2]);
      if (current?.kind === "CustomResourceDefinition") (current.status ??= {}).conditions = [
        { type: "Established", status: options.established ? "True" : "False" },
      ];
      return { stdout: current ? JSON.stringify(current) : "" };
    }
    if (["create", "apply"].includes(args[0])) {
      if (args.some(arg => arg.startsWith("--force"))) throw new Error("No forced schema writes");
      const desired = JSON.parse(settings.input!);
      beforeWrite(desired);
      const objectKey = desired.kind === "CustomResourceDefinition" ? desired.metadata.name : `${desired.kind.toLowerCase()}/${desired.metadata.name}`;
      const current = objects.get(objectKey);
      if (args[0] === "create" && current) throw new Error("409 create conflict");
      if (args[0] === "apply" && (!current || desired.metadata.uid !== current.metadata.uid
        || desired.metadata.resourceVersion !== current.metadata.resourceVersion)) throw new Error("409 UID/resourceVersion conflict");
      const applied = structuredClone(desired);
      applied.metadata = { ...current?.metadata, ...desired.metadata,
        labels: { ...current?.metadata.labels, ...desired.metadata.labels },
        annotations: { ...current?.metadata.annotations, ...desired.metadata.annotations },
        uid: current?.metadata.uid ?? `${desired.metadata.name}-uid`, resourceVersion: String(revision++),
        generation: (current?.metadata.generation ?? 0) + 1 };
      applied.status = { storedVersions: ["v1alpha1"], conditions: [] };
      objects.set(objectKey, applied);
      writes.push(structuredClone(applied));
      return { stdout: JSON.stringify(applied) };
    }
    throw new Error(`Unexpected schema fixture request ${args.join(" ")}`);
  };
  return { owner, objects, requests, writes, options, execute, install,
    wait: { timeoutMs: 1500, now: () => time, sleep: async (ms: number) => { time += ms; onSleep(); } },
    onSleep: (callback: () => void) => { onSleep = callback; },
    beforeWrite: (callback: (object: ObjectMap) => void) => { beforeWrite = callback; },
    beforeRaw: (callback: (path: string) => void) => { beforeRaw = callback; },
  };
}
