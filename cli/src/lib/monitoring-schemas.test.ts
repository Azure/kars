// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { beforeEach, describe, expect, it, vi } from "vitest";
import { coreSchemaDocuments, planMonitoringSchemas } from "./monitoring-schemas.js";
import { normalizedCrd, type ObjectMap, type SchemaExecute } from "./schema-documents.js";
import { waitForPublishedSchemas } from "./schema-discovery.js";

vi.mock("./schema-discovery.js", () => ({ waitForPublishedSchemas: vi.fn() }));
const desired: ObjectMap = {
  apiVersion: "apiextensions.k8s.io/v1", kind: "CustomResourceDefinition",
  metadata: { name: "podmonitors.monitoring.coreos.com" },
  spec: { group: "monitoring.coreos.com", scope: "Namespaced",
    names: { plural: "podmonitors", kind: "PodMonitor" },
    versions: [{ name: "v1", served: true, storage: true, schema: { openAPIV3Schema: { type: "object" } } }] },
};
const live = (): ObjectMap => ({ ...structuredClone(desired), metadata: { ...desired.metadata,
  uid: "live", resourceVersion: "1", annotations: { "platform.example/owner": "external" } },
  status: { conditions: [{ type: "Established", status: "True" }] },
});
function fixture(current?: ObjectMap) {
  const state = { current, previewMismatch: false };
  const writes: string[][] = [];
  const execute: SchemaExecute = async (_file, args, options) => {
    if (args[0] === "get") return { stdout: state.current ? JSON.stringify(state.current) : "" };
    if (args[0] === "create" && args.includes("--dry-run=server")) {
      const preview = JSON.parse(options.input!);
      preview.metadata.uid = "dry-run";
      if (state.previewMismatch) preview.spec.scope = "Cluster";
      return { stdout: JSON.stringify(preview) };
    }
    if (args[0] === "create") {
      writes.push([...args]);
      state.current = live();
      return { stdout: JSON.stringify(state.current) };
    }
    throw new Error("Unexpected schema command");
  };
  return { execute, writes, state };
}
beforeEach(() => {
  vi.mocked(waitForPublishedSchemas).mockReset();
  vi.mocked(waitForPublishedSchemas).mockImplementation(async (_execute, _types, check) => {
    expect(await check()).toBe(true);
  });
});
describe("separate monitoring CRD lifecycle", () => {
  it("keeps the Kars normalizer strict and partitions only recognized dependency names", () => {
    expect(() => normalizedCrd(desired)).toThrow("unsupported");
    expect(coreSchemaDocuments([desired])).toEqual([]);
    const foreign = { ...desired, metadata: { name: "unknown.monitoring.coreos.com" } };
    expect(coreSchemaDocuments([foreign])).toEqual([foreign]);
  });
  it("previews missing definitions, creates once and verifies publication before returning", async () => {
    const f = fixture();
    const apply = await planMonitoringSchemas(f.execute, [desired]);
    expect(f.writes).toEqual([]);
    await apply();
    expect(f.writes).toHaveLength(1);
    expect(f.writes[0]).not.toContain("--force-conflicts");
    expect(waitForPublishedSchemas).toHaveBeenCalledTimes(1);
  });
  it("reuses an exact external schema without adoption or metadata changes", async () => {
    const current = live();
    const f = fixture(current);
    await (await planMonitoringSchemas(f.execute, [desired]))();
    expect(f.writes).toEqual([]);
    expect(f.state.current).toEqual(current);
  });
  it("rejects existing schema differences without overwrite", async () => {
    const current = live();
    current.spec.versions[0].schema.openAPIV3Schema.additionalProperties = false;
    const f = fixture(current);
    await expect(planMonitoringSchemas(f.execute, [desired])).rejects.toThrow("upgrade that dependency explicitly");
    expect(f.writes).toEqual([]);
  });
  it("refuses concurrent replacement after planning", async () => {
    const f = fixture(live());
    const apply = await planMonitoringSchemas(f.execute, [desired]);
    f.state.current!.metadata.uid = "replacement";
    await expect(apply()).rejects.toThrow("changed after review");
    expect(f.writes).toEqual([]);
  });
  it("rejects server preview changes before any persistence", async () => {
    const f = fixture();
    f.state.previewMismatch = true;
    await expect(planMonitoringSchemas(f.execute, [desired])).rejects.toThrow("preview differs");
    expect(f.writes).toEqual([]);
  });
  it("does not bootstrap dependencies in check-only mode", async () => {
    const f = fixture();
    await expect(planMonitoringSchemas(f.execute, [desired], { checkOnly: true })).rejects.toThrow("not installed");
    expect(f.writes).toEqual([]);
  });
  it("rejects duplicate definitions and live identities", async () => {
    const f = fixture();
    await expect(planMonitoringSchemas(f.execute, [desired, desired])).rejects.toThrow("Duplicate");
    await expect(planMonitoringSchemas(f.execute, [live()])).rejects.toThrow("live object identities");
    expect(f.writes).toEqual([]);
  });
});
