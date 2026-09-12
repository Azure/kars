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
});
