// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { schemaIdentity, schemaOwnerFields, verifyNewSchemaPreviewOwner, verifySchemaOwner } from "./schema-documents.js";
import { crd } from "./schema-stage.test-support.js";

const owner = { release: "kars", namespace: "kars-system", ownership: "helm" as const };
function preview() {
  const object = crd("KarsBudgetAccount", "karsbudgetaccounts");
  object.metadata = { ...object.metadata, ...schemaOwnerFields(owner), uid: "ephemeral-preview-uid" };
  return object;
}

describe("new CRD server-preview identity", () => {
  it.each([undefined, ""])("accepts only non-persisted preview RV=%s without inventing a revision", version => {
    const object = preview();
    if (version !== undefined) object.metadata.resourceVersion = version;
    const before = structuredClone(object);
    expect(() => verifyNewSchemaPreviewOwner(object, owner)).not.toThrow();
    expect(object).toEqual(before);
    expect(() => schemaIdentity(object)).toThrow("live UID/resourceVersion");
    expect(() => verifySchemaOwner(object, owner)).toThrow("live UID/resourceVersion");
  });

  it.each(["27", "0", null, 27])("refuses a persisted/invalid preview revision %s", value => {
    const object = preview();
    object.metadata.resourceVersion = value;
    expect(() => verifyNewSchemaPreviewOwner(object, owner)).toThrow("no persisted resourceVersion");
  });

  it.each([
    (object: ReturnType<typeof preview>) => { delete object.metadata.uid; },
    (object: ReturnType<typeof preview>) => { object.metadata.uid = ""; },
    (object: ReturnType<typeof preview>) => { object.metadata.namespace = "other"; },
    (object: ReturnType<typeof preview>) => { object.metadata.deletionTimestamp = "2026-09-12T00:00:00Z"; },
    (object: ReturnType<typeof preview>) => { object.metadata.ownerReferences = [{ uid: "foreign" }]; },
    (object: ReturnType<typeof preview>) => { object.metadata.annotations["meta.helm.sh/release-name"] = "foreign"; },
    (object: ReturnType<typeof preview>) => { object.metadata.name = "other.kars.azure.com"; },
  ])("keeps exact schema/ownership checks on previews: %#", change => {
    const object = preview();
    change(object);
    expect(() => verifyNewSchemaPreviewOwner(object, owner)).toThrow();
  });

  it("still requires a real revision for reads, updates and publication", () => {
    const object = preview();
    object.metadata.resourceVersion = "27";
    expect(() => verifySchemaOwner(object, owner)).not.toThrow();
    expect(schemaIdentity(object)).toEqual({ uid: "ephemeral-preview-uid", resourceVersion: "27" });
    expect(() => verifyNewSchemaPreviewOwner(object, owner)).toThrow();
  });
});
