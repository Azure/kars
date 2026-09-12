// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { schemaSsaConflict } from "./schema-ssa-conflicts.js";
import { schemaStep, withSchemaPreparationDiagnostics } from "./sre-schema-diagnostics.js";

describe("kubectl SSA conflict diagnostics", () => {
  it("distinguishes a witnessed resourceVersion precondition failure from field ownership", () => {
    expect(schemaSsaConflict('error: Operation cannot be fulfilled on customresourcedefinitions.apiextensions.k8s.io "PRIVATE-NAME": the object has been modified; please apply your changes to the latest version and try again\n'))
      .toEqual({ conflictKind: "resource-version" });
  });
  it("recognizes the pinned single-conflict wrapper without disclosing manager time or raw fields", () => {
    const stderr = 'error: Apply failed with 1 conflict: conflict with "python-httpx" using apiextensions.k8s.io/v1 at 2026-09-12T00:00:00Z: .spec.versions\n'
      + "Please review the fields above--they currently have other managers. Here\nPRIVATE-BODY";
    expect(schemaSsaConflict(stderr)).toEqual({ conflictKind: "field-manager", conflictCount: 1,
      conflictFields: ["spec/versions"], conflictManagers: ["python-httpx"] });
  });

  it("bounds multi-manager conflicts and maps unreviewed manager/field text to fixed classes", () => {
    expect(schemaSsaConflict('error: Apply failed with 2 conflicts: conflicts with "PRIVATE-MANAGER":\n'
      + '- .spec.PRIVATE-FIELD\nconflicts with "helm" using apiextensions.k8s.io/v1:\n- .spec.versions\n'))
      .toEqual({ conflictKind: "field-manager", conflictCount: 2, conflictFields: ["other", "spec/versions"],
        conflictManagers: ["helm", "other"] });
  });

  it.each([
    'Error from server (Conflict): resourceVersion changed',
    'error: some OTHER failure mentioning Apply failed with 1 conflict: conflict with "helm": .spec.versions',
    'error: Apply failed with 2 conflicts: conflict with "helm": .spec.versions',
    'error: Apply failed with 33 conflicts: conflict with "helm": .spec.versions',
    'error: Apply failed with 1 conflict: unrecognized PRIVATE format',
  ])("does not misclassify unrelated, malformed or unbounded failures", stderr => {
    expect(schemaSsaConflict(stderr)).toBeUndefined();
  });

  it("preserves failure and emits only fixed conflict facts at the existing Task preview step", async () => {
    const output = vi.spyOn(console, "error").mockImplementation(() => {});
    try {
      await expect(withSchemaPreparationDiagnostics(async () => {
        schemaStep("schema-server-preview", "KarsTask");
        throw Object.assign(new Error("PRIVATE-STDOUT"), { exitCode: 1,
          stderr: 'error: Apply failed with 1 conflict: conflict with "PRIVATE-MANAGER": .spec.versions\n' });
      })).rejects.toThrow("schema-server-preview: Conflict");
      const facts = JSON.parse(String(output.mock.calls[0][0]).replace("SRE-SCHEMA-PREPARATION ", ""));
      expect(facts).toMatchObject({ kind: "KarsTask", category: "api-rejection", reason: "Conflict",
        conflictKind: "field-manager", conflictFields: ["spec/versions"], conflictManagers: ["other"] });
      expect(JSON.stringify(output.mock.calls)).not.toContain("PRIVATE");
    } finally { output.mockRestore(); }
  });
});
