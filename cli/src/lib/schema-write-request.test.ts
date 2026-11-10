// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { crd, schemaFixture } from "./schema-stage.test-support.js";
import { normalizedCrd, SCHEMA_DIGEST, SCHEMA_OWNER, schemaDigest } from "./schema-documents.js";
import { stageCoreSchemaDocuments } from "./schema-stage.js";
import { buildSchemaWriteRequest, verifySchemaWritePreview } from "./schema-write-request.js";

describe("shared production schema request and preview checks", () => {
  it("matches every production request byte, owner annotation and JS digest", async () => {
    const desired = crd("KarsTask", "karstasks");
    const f = schemaFixture([desired]);
    const current = structuredClone(f.install(desired));
    desired.spec.versions[0].schema.openAPIV3Schema.properties.spec.properties.numeric = {
      type: "number", minimum: 1e-7, maximum: 1e21,
    };
    const expected = buildSchemaWriteRequest(desired, current, f.owner);
    expect(expected.object.metadata.annotations[SCHEMA_OWNER])
      .toBe('{"namespace":"kars-system","ownership":"helm","release":"kars"}');
    expect(expected.object.metadata.annotations[SCHEMA_DIGEST]).toBe(schemaDigest(normalizedCrd(desired)));
    expect(expected.object.metadata).toMatchObject({ uid: current.metadata.uid, resourceVersion: current.metadata.resourceVersion });
    await stageCoreSchemaDocuments(f.execute, [desired], { ...f.owner, ...f.wait });
    const writes = f.requests.filter(request => request.args[0] === "apply");
    expect(writes).toHaveLength(1);
    expect(writes[0].args).toEqual([...expected.args, "--request-timeout=20s"]);
    expect(writes[0].input).toBe(JSON.stringify(expected.object));
  });

  it.each(["schema-addition", "owner", "uid", "missing-rv"])("does not replace exact preview checks with containment: %s", fault => {
    const desired = crd("KarsTask", "karstasks");
    const f = schemaFixture([desired]);
    const current = f.install(desired);
    const checked = buildSchemaWriteRequest(desired, current, f.owner).object;
    if (fault === "schema-addition") checked.spec = structuredClone(checked.spec);
    if (fault === "schema-addition") checked.spec.versions[0].schema.openAPIV3Schema.properties.extra = { type: "string" };
    if (fault === "owner") checked.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
    if (fault === "uid") checked.metadata.uid = "replacement";
    if (fault === "missing-rv") delete checked.metadata.resourceVersion;
    expect(() => verifySchemaWritePreview(checked, desired, f.owner, current.metadata.uid)).toThrow();
  });

  it("retains only the existing normalized defaults, not extra validation or fields", () => {
    const desired = crd("KarsTask", "karstasks");
    const f = schemaFixture([desired]);
    const current = f.install(desired);
    const checked = structuredClone(buildSchemaWriteRequest(desired, current, f.owner).object);
    checked.spec.names.listKind = "KarsTaskList";
    checked.spec.conversion = { strategy: "None" };
    expect(() => verifySchemaWritePreview(checked, desired, f.owner, current.metadata.uid)).not.toThrow();
    checked.spec.versions[0].schema.openAPIV3Schema.properties.extra = { type: "string" };
    expect(() => verifySchemaWritePreview(checked, desired, f.owner, current.metadata.uid)).toThrow("another identity or schema");
  });

  it("uses an identity-fenced replacement only for the same Helm manager's legacy spec updates", async () => {
    const desired = crd();
    const f = schemaFixture([desired]);
    const current = f.install(desired);
    current.metadata.managedFields = [
      { manager: "helm", operation: "Apply", apiVersion: desired.apiVersion,
        fieldsType: "FieldsV1", fieldsV1: { "f:spec": { "f:versions": {} } } },
      { manager: "helm", operation: "Update", apiVersion: desired.apiVersion,
        fieldsType: "FieldsV1", fieldsV1: { "f:spec": { "f:versions": {} } } },
      { manager: "kube-apiserver", operation: "Update", apiVersion: desired.apiVersion,
        fieldsType: "FieldsV1", fieldsV1: { "f:status": {} } },
    ];
    current.metadata.labels.operator = "retained";
    current.metadata.annotations.operator = "retained";
    current.metadata.finalizers = ["example.com/retain"];
    const previous = structuredClone(current);
    const request = buildSchemaWriteRequest(desired, current, f.owner);
    expect(request.args).toEqual(["replace", "--field-manager=helm", "-f", "-", "-o", "json"]);
    expect(request.object.metadata).toMatchObject({
      uid: current.metadata.uid, resourceVersion: current.metadata.resourceVersion,
      labels: { operator: "retained" }, annotations: { operator: "retained" },
      finalizers: current.metadata.finalizers, managedFields: current.metadata.managedFields,
    });
    expect(request.object.status).toEqual(current.status);
    expect(request.object.spec).toEqual(desired.spec);
    expect(current).toEqual(previous);
    expect(() => verifySchemaWritePreview(request.object, desired, f.owner, current.metadata.uid)).not.toThrow();
    desired.spec.versions[0].schema.openAPIV3Schema.description = "Updated owned description";
    await expect(stageCoreSchemaDocuments(f.execute, [desired], { ...f.owner, ...f.wait }))
      .resolves.toMatchObject({ schemas: 1, published: true });
    const replacements = f.requests.filter(item => item.args[0] === "replace");
    expect(replacements).toHaveLength(2);
    expect(replacements[0].args).toContain("--dry-run=server");
    expect(replacements[1].args).not.toContain("--dry-run=server");
    expect(f.requests.filter(item => item.args[0] === "get").some(item => item.args.includes("--show-managed-fields"))).toBe(true);
  });

  it.each(["foreign-manager", "foreign-api", "unknown-operation", "unknown-fields", "apply-only", "template-owner"])(
    "does not replace SSA conflict protection for %s", fault => {
      const desired = crd();
      const f = schemaFixture([desired]);
      const current = f.install(desired);
      current.metadata.managedFields = [{
        manager: "helm", operation: "Update", apiVersion: desired.apiVersion,
        fieldsType: "FieldsV1", fieldsV1: { "f:spec": { "f:versions": {} } },
      }];
      const owner = { ...f.owner };
      if (fault === "foreign-manager") current.metadata.managedFields.push({
        ...current.metadata.managedFields[0], manager: "operator",
      });
      if (fault === "foreign-api") current.metadata.managedFields[0].apiVersion = "apiextensions.k8s.io/v1beta1";
      if (fault === "unknown-operation") current.metadata.managedFields[0].operation = "Unknown";
      if (fault === "unknown-fields") current.metadata.managedFields[0].fieldsType = "Unknown";
      if (fault === "apply-only") current.metadata.managedFields[0].operation = "Apply";
      if (fault === "template-owner") owner.ownership = "template";
      const request = buildSchemaWriteRequest(desired, current, owner);
      expect(request.args[0]).toBe("apply");
      expect(request.args).toContain("--server-side");
      expect(request.args.some(arg => arg.includes("force"))).toBe(false);
    });

  it.each(["uid", "resourceVersion"])("rejects a %s race on the legacy Helm replacement path", async field => {
    const desired = crd();
    const f = schemaFixture([desired]);
    const current = f.install(desired);
    current.metadata.managedFields = [{
      manager: "helm", operation: "Update", apiVersion: desired.apiVersion,
      fieldsType: "FieldsV1", fieldsV1: { "f:spec": { "f:versions": {} } },
    }];
    desired.spec.versions[0].schema.openAPIV3Schema.description = "Updated owned description";
    f.beforeWrite(() => { current.metadata[field] = "changed"; });
    await expect(stageCoreSchemaDocuments(f.execute, [desired], { ...f.owner, ...f.wait }))
      .rejects.toThrow("409 UID/resourceVersion conflict");
    expect(f.writes).toEqual([]);
  });
});
