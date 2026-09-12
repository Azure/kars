// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import {
  canonicalSchema, normalizedCrd, schemaDocuments, schemaIdentity, verifySchemaOwner,
} from "../../../cli/dist/lib/schema-documents.js";

const owner = { namespace: "kars-system", release: "kars", ownership: "helm" };
const name = "karstasks.kars.azure.com";
const editedPaths = [["spec", "versions"], ["metadata", "annotations", "meta.helm.sh/release-name"]];

function require(value, message) {
  if (!value) throw new Error(message);
}

function map(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function entries(object) {
  const result = object.metadata.managedFields;
  require(Array.isArray(result) && result.length > 0 && result.length <= 64, "Unbounded or missing managedFields");
  require(result.every(entry => map(entry) && map(entry.fieldsV1)), "Invalid managedFields entry");
  return result;
}

function originalApply(entry) {
  return entry.manager === "helm" && entry.operation === "Apply" && entry.fieldsType === "FieldsV1"
    && entry.apiVersion === "apiextensions.k8s.io/v1"
    && (entry.subresource === undefined || entry.subresource === "");
}

function fieldAt(tree, path) {
  let value = tree;
  for (const key of path) value = map(value) ? value[`f:${key}`] : undefined;
  return value;
}

function overlaps(tree, path) {
  require(map(tree), "Invalid managedFields tree");
  if (Object.keys(tree).length === 0 || Object.hasOwn(tree, ".") || path.length === 0) return true;
  const next = tree[`f:${path[0]}`];
  return next !== undefined && overlaps(next, path.slice(1));
}

function verifiedOriginal(original) {
  normalizedCrd(original);
  verifySchemaOwner(original, owner);
  require(original.metadata.name === name, "Another CRD entered the Task fixture");
  const rows = entries(original);
  const selected = rows.filter(originalApply);
  require(selected.length === 1, "Fixture requires one original Helm Apply identity, not Update or another API version");
  for (const path of editedPaths) {
    const claim = fieldAt(selected[0].fieldsV1, path);
    require(map(claim) && Object.keys(claim).length === 0, "Fixture edits require complete original field ownership");
    require(!rows.some(row => row !== selected[0] && overlaps(row.fieldsV1, path)),
      "Fixture edits are co-owned or owned by another operation");
  }
  return selected[0];
}

function ownershipState(object) {
  return entries(object).map(entry => {
    const value = structuredClone(entry);
    if (originalApply(entry)) delete value.time;
    return canonicalSchema(value);
  }).sort();
}

function values(object) {
  return { spec: object.spec, metadata: Object.fromEntries(Object.entries(object.metadata)
    .filter(([key]) => !["resourceVersion", "managedFields", "generation"].includes(key))) };
}

function changedValue(original, fault) {
  require(fault === "owner" || fault === "schema", "Unknown fixture change");
  const changed = structuredClone(original);
  if (fault === "owner") changed.metadata.annotations["meta.helm.sh/release-name"] = "foreign-fixture";
  else changed.spec.versions[0].schema.openAPIV3Schema.description = "Unreviewed public fixture description";
  return changed;
}

function verifyState(original, current, fault, changed) {
  require(fault === "owner" || fault === "schema", "Unknown fixture change");
  verifiedOriginal(original);
  schemaIdentity(current);
  require(current.metadata.uid === original.metadata.uid && current.metadata.name === name,
    "Task UID changed during the fixture");
  require(canonicalSchema(ownershipState(current)) === canonicalSchema(ownershipState(original)),
    "Unplanned managedFields drift; no ownership recovery is permitted");
  const expected = changed ? changedValue(original, fault) : original;
  require(canonicalSchema(values(current)) === canonicalSchema(values(expected)),
    "Unplanned Task value change; no restoration is permitted");
}

function project(fields, live, manifest, path = [], budget = { nodes: 0 }) {
  require(map(fields) && ++budget.nodes <= 4096 && path.length <= 32, "Unsupported owned field tree");
  if (Object.keys(fields).length === 0) {
    if (live !== undefined) return structuredClone(live);
    // API-omitted empty/default spec values come only from the matching Helm
    // manifest, never a guessed default. Metadata cannot use this fallback.
    require(path[0] === "spec" && manifest !== undefined, "An original owned value is unavailable");
    return structuredClone(manifest);
  }
  require(map(live) || map(manifest), "Owned map is unavailable");
  const result = {};
  for (const [field, children] of Object.entries(fields)) {
    if (field === ".") {
      require(map(children) && Object.keys(children).length === 0, "Invalid owned map marker");
      continue;
    }
    require(field.startsWith("f:") && field.length > 2, "Indexed ownership is outside the Task fixture");
    const key = field.slice(2);
    require(key !== "__proto__" && key !== "constructor" && key !== "prototype", "Unsupported field key");
    result[key] = project(children, map(live) ? live[key] : undefined,
      map(manifest) ? manifest[key] : undefined, [...path, key], budget);
  }
  return result;
}

export function verifyOwnedTaskState(input) {
  require(input.state === "changed" || input.state === "restored", "Unknown fixture state");
  verifyState(input.original, input.current, input.fault, input.state === "changed");
  return { verified: true };
}

export function ownedTaskRequest(input) {
  const selected = verifiedOriginal(input.original);
  require(typeof input.restore === "boolean" && typeof input.manifest === "string", "Missing fixture request context");
  verifyState(input.original, input.current, input.fault, input.restore);
  const manifests = schemaDocuments(input.manifest).filter(object =>
    object.kind === "CustomResourceDefinition" && object.metadata.name === name);
  require(manifests.length === 1
    && canonicalSchema(normalizedCrd(manifests[0])) === canonicalSchema(normalizedCrd(input.original)),
  "Original Task spec differs from its Helm release");
  const payload = project(selected.fieldsV1, input.original, manifests[0]);
  require(Object.keys(payload).every(key => ["apiVersion", "kind", "metadata", "spec"].includes(key))
    && map(payload.metadata) && map(payload.spec), "Unsupported original Helm fields");
  require(!["uid", "resourceVersion", "managedFields", "generation", "creationTimestamp", "deletionTimestamp"]
    .some(key => Object.hasOwn(payload.metadata, key)), "Server identity fields cannot be managed by the fixture");
  if (!input.restore) {
    if (input.fault === "owner") payload.metadata.annotations["meta.helm.sh/release-name"] = "foreign-fixture";
    else payload.spec.versions[0].schema.openAPIV3Schema.description = "Unreviewed public fixture description";
  }
  payload.apiVersion = input.original.apiVersion;
  payload.kind = input.original.kind;
  payload.metadata = { ...payload.metadata, name, ...schemaIdentity(input.current) };
  return { args: ["apply", "--server-side", "--field-manager=helm", "-f", "-", "-o", "json"],
    input: JSON.stringify(payload) };
}
