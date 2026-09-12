// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { CANONICAL_SCHEMAS, EVALUATOR_V2 } from "./sre-migration-catalog.js";
import { canonicalSchema, normalizedCrd, schemaDigest, schemaDocuments, schemaOwnerFields, type ObjectMap, type SchemaExecute } from "./schema-documents.js";
import { schemaFixture } from "./schema-stage.test-support.js";

const BUDGET_RULE = "UnsupportedLaunchBudget: total/subtree token and usdMicros ceilings are not enforced; bounded tasks may be planned but cannot launch";
const BUDGET_EXPRESSION = "!has(self.execution) || !self.execution.launch || !has(self.envelope.budget) || ((!has(self.envelope.budget.tokens) || self.envelope.budget.tokens == 0) && (!has(self.envelope.budget.usdMicros) || self.envelope.budget.usdMicros == 0))";

export function canonicalMigrationSchemas(evalV2 = false): { before: ObjectMap[]; after: ObjectMap[] } {
  const chart = fileURLToPath(new URL("../../../deploy/helm/kars", import.meta.url));
  const after = schemaDocuments(execFileSync("helm", ["template", "kars", chart, "--namespace", "kars-system",
    "--set", "sre.enabled=false", "--dry-run=client"], { encoding: "utf8" }))
    .filter(object => object.kind === "CustomResourceDefinition");
  const evalCrd = after.find(object => object.spec.names.kind === "KarsEval")!;
  if (!CANONICAL_SCHEMAS[evalCrd.metadata.name].after.includes(schemaDigest(normalizedCrd(evalCrd)))) {
    throw new Error("Current evaluator schema is not a qualified fixture input");
  }
  const evalSchema = evalCrd.spec.versions[0].schema.openAPIV3Schema;
  for (const key of ["reportConfigMapRef", "reportConfigMapUid", "reportEvidenceDigest"]) {
    delete evalSchema.properties.status.properties[key];
  }
  if (evalV2) Object.assign(evalSchema.properties.status.properties, {
    reportConfigMapRef: { description: "Bounded, exclusively owned latest per-case report and attribution.", nullable: true,
      properties: { name: { type: "string" } }, required: ["name"], type: "object" },
    reportConfigMapUid: { description: "API UID of the report ConfigMap verified before publishing this status.", nullable: true, type: "string" },
    reportEvidenceDigest: { description: "Digest of evidence already verified by this controller; fences cache-only replay.", nullable: true, type: "string" },
  });
  const before: ObjectMap[] = [];
  for (const target of after) {
    const entry = CANONICAL_SCHEMAS[target.metadata.name];
    if (!entry || !entry.after.includes(schemaDigest(normalizedCrd(target)))) throw new Error(`Unqualified canonical target fixture: ${target.metadata.name}`);
    if (!entry.before) continue;
    const object = structuredClone(target);
    const root = object.spec.versions[0].schema.openAPIV3Schema;
    const spec = root.properties.spec;
    const removeBindings = (blueprint: ObjectMap | undefined) => {
      if (blueprint?.properties) {
        delete blueprint.properties.credentialBindings;
        delete blueprint.properties.githubBinding;
      }
    };
    const removeScope = (envelope: ObjectMap | undefined) => {
      if (envelope?.properties?.budget?.properties) delete envelope.properties.budget.properties.scope;
    };
    const kind = object.spec.names.kind;
    if (kind === "KarsProfile") removeScope(spec.properties.defaultEnvelope);
    if (kind === "KarsTask" || kind === "KarsTeam") {
      removeBindings(spec.properties.blueprint);
      removeScope(spec.properties.envelope);
      spec["x-kubernetes-validations"] = spec["x-kubernetes-validations"].filter((rule: ObjectMap) =>
        !rule.message.startsWith("First finite GovernedInference") && !rule.message.startsWith("GovernedInference scope cannot"));
      if (kind === "KarsTask") {
        const budget = spec["x-kubernetes-validations"].find((rule: ObjectMap) => rule.message.startsWith("UnsupportedLaunchBudget"));
        budget.rule = BUDGET_EXPRESSION;
        budget.message = BUDGET_RULE;
        delete root.properties.status.properties.inferenceBudget;
      } else {
        removeBindings(spec.properties.roster.items.properties.blueprint);
        removeScope(spec.properties.roster.items.properties.envelope);
        delete root.properties.status.properties.inferenceBudgetAccount;
      }
    }
    if (kind === "McpServer") {
      delete spec.properties.managed;
      spec["x-kubernetes-validations"] = spec["x-kubernetes-validations"].filter((rule: ObjectMap) => !rule.message.startsWith("spec.managed"));
      const exclusive = spec["x-kubernetes-validations"].find((rule: ObjectMap) => rule.message.startsWith("spec.bundleRef is mutually"));
      exclusive.message = "spec.bundleRef is mutually exclusive with spec.url, spec.oauth, spec.productionMode, spec.scopes, spec.allowedTools, and spec.displayName";
      exclusive.rule = "!has(self.bundleRef) || (!has(self.url) && !has(self.oauth) && !has(self.productionMode) && !has(self.scopes) && !has(self.allowedTools) && !has(self.displayName))";
      for (const key of ["discoveredTools", "endpoint", "managedNamespaceUid", "mode", "toolSchemaDigest", "workloadGeneration", "workloadImage", "workloadRef"]) {
        delete root.properties.status.properties[key];
      }
    }
    if (kind === "KarsSandbox") {
      spec.properties.credentialsRef.properties.name.pattern = "^kars-credential-source-[a-z0-9][a-z0-9-]*$";
      removeBindings(spec);
      delete spec.properties.inferenceBudgetRef;
      delete root.properties.status.properties.serviceObservation;
    }
    if (kind === "KarsSREAction") {
      const params = spec.properties.action.properties.params;
      delete params["x-kubernetes-preserve-unknown-fields"];
      params.additionalProperties = true;
    }
    if (kind === "KarsEval") for (const key of ["reportConfigMapRef", "reportConfigMapUid", "reportEvidenceDigest"]) delete root.properties.status.properties[key];
    if (schemaDigest(normalizedCrd(object)) !== entry.before) throw new Error(`Historical fixture does not match BASE365: ${object.metadata.name}`);
    delete object.metadata.annotations?.["helm.sh/resource-policy"];
    before.push(object);
  }
  if (evalV2 && schemaDigest(normalizedCrd(after.find(object => object.spec.names.kind === "KarsEval")!)) !== EVALUATOR_V2) throw new Error("Evaluator-v2 fixture drifted");
  return { before, after };
}

export function migrationFixture(evalV2 = false) {
  const { before, after } = canonicalMigrationSchemas(evalV2);
  const base = schemaFixture(after);
  base.objects.set("kars-controller", { kind: "Deployment", metadata: { name: "kars-controller", namespace: "kars-system",
    uid: "controller-uid", resourceVersion: "2", ...schemaOwnerFields(base.owner) },
    spec: { replicas: 0, template: { spec: { serviceAccountName: "kars-controller" } } }, status: {} });
  for (const object of before) base.install(object);
  const data = new Map<string, ObjectMap[]>();
  const validations: string[] = [];
  const requests: { args: readonly string[]; input?: string }[] = [];
  let onDataRead = (_name: string) => {};
  let onDryRun = (_object: ObjectMap) => {};
  const execute: SchemaExecute = async (file, args, options) => {
    requests.push({ args, input: options.input });
    if (file === "kubectl" && args[0] === "auth") return { stdout: "yes" };
    if (file === "kubectl" && args[0] === "get" && args[1] === "pods") return { stdout: '{"metadata":{},"items":[]}' };
    if (file === "helm" && args[0] === "get" && args[1] === "manifest") return { stdout: before.map(object => JSON.stringify(object)).join("\n---\n") };
    if (file === "kubectl" && args[0] === "get" && args.includes("--raw") && args[args.indexOf("--raw") + 1].includes("?limit=")) {
      const plural = args[args.indexOf("--raw") + 1].split("?")[0].split("/").at(-1)!;
      onDataRead(plural);
      return { stdout: JSON.stringify({ metadata: {}, items: data.get(plural) ?? [] }) };
    }
    if (file === "kubectl" && args[0] === "replace") {
      if (!args.some(arg => arg.endsWith("?dryRun=All"))) throw new Error("Fixture forbids real data writes");
      const object = JSON.parse(options.input!);
      validations.push(object.metadata.uid);
      return { stdout: JSON.stringify(object) };
    }
    if (file === "kubectl" && args.includes("--dry-run=server")) {
      const object = JSON.parse(options.input!);
      onDryRun(object);
      return { stdout: JSON.stringify({ ...object, metadata: { ...object.metadata,
        uid: object.metadata.uid ?? "dry-run-uid", resourceVersion: object.metadata.resourceVersion ?? "dry-run-version" } }) };
    }
    return base.execute(file, args, options);
  };
  const addData = (kind: string, spec: ObjectMap, status?: ObjectMap) => {
    const crd = before.find(object => object.spec.names.kind === kind)!;
    const object = { apiVersion: "kars.azure.com/v1alpha1", kind,
      metadata: { name: `${kind.toLowerCase()}-fixture`, namespace: "kars-system", uid: `${kind}-uid`, resourceVersion: "1" },
      spec, ...(status ? { status } : {}) };
    data.set(crd.spec.names.plural, [...(data.get(crd.spec.names.plural) ?? []), object]);
    return object;
  };
  return { ...base, execute, before, after, data, validations, requests, addData,
    snapshot: () => canonicalSchema([...data]),
    onDataRead: (callback: typeof onDataRead) => { onDataRead = callback; },
    onDryRun: (callback: typeof onDryRun) => { onDryRun = callback; },
  };
}
