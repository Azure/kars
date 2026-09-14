// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import { schemaField, schemaStep, withSchemaPreparationDiagnostics } from "./sre-schema-diagnostics.js";

afterEach(() => vi.restoreAllMocks());

describe("SRE schema preparation diagnostic boundary", () => {
  it("exposes only fixed child/source/shape facts, not values or original error causes", async () => {
    const output = vi.spyOn(console, "error").mockImplementation(() => {});
    const secret = "PRIVATE-CR-TOKEN-CERTIFICATE";
    const original = Object.assign(new Error(secret), {
      stderr: `Error from server (BadRequest): ${secret}`, stdout: secret, cause: { token: secret },
    });
    await expect(withSchemaPreparationDiagnostics(async () => {
      schemaStep("schema-preview-identity", "KarsBudgetAccount", {
        apiVersion: secret, kind: secret, metadata: { uid: secret },
      });
      schemaField(secret);
      throw original;
    })).rejects.toThrow("schema-preview-identity: BadRequest");
    const report = JSON.parse(String(output.mock.calls[0][0]).replace("SRE-SCHEMA-PREPARATION ", ""));
    expect(report).toEqual({
      step: "schema-preview-identity", source: "cli/src/lib/schema-stage.ts", kind: "KarsBudgetAccount",
      field: "unrecognized", shape: { uid: "string", resourceVersion: "missing", kind: "string", apiVersion: "string" },
      category: "api-rejection", reason: "BadRequest",
    });
    expect(JSON.stringify(output.mock.calls)).not.toContain(secret);
  });

  it("reports a missing live item TypeMeta without logging the item or namespace", async () => {
    const output = vi.spyOn(console, "error").mockImplementation(() => {});
    await expect(withSchemaPreparationDiagnostics(async () => {
      schemaStep("data-item-identity", "KarsTask", { metadata: { uid: "private", resourceVersion: "private", namespace: "private" } });
      throw new Error("private");
    })).rejects.toThrow("data-item-identity: local-check");
    const facts = JSON.parse(String(output.mock.calls[0][0]).replace("SRE-SCHEMA-PREPARATION ", ""));
    expect(facts.shape).toEqual({ uid: "string", resourceVersion: "string", kind: "missing", apiVersion: "missing" });
    expect(JSON.stringify(output.mock.calls)).not.toContain("private");
  });

  it("isolates concurrent preparation traces and does not change successful return values", async () => {
    const output = vi.spyOn(console, "error").mockImplementation(() => {});
    const results = await Promise.allSettled(["KarsTask", "KarsTeam"].map(kind => withSchemaPreparationDiagnostics(async () => {
      schemaStep("data-server-validation", kind);
      await Promise.resolve();
      throw new Error("not retained");
    })));
    expect(results.every(result => result.status === "rejected")).toBe(true);
    const kinds = output.mock.calls.map(call => JSON.parse(String(call[0]).replace("SRE-SCHEMA-PREPARATION ", "")).kind);
    expect(kinds.sort()).toEqual(["KarsTask", "KarsTeam"]);
    output.mockClear();
    schemaStep("data-inventory", "untrusted-kind");
    const result = { retained: true };
    expect(await withSchemaPreparationDiagnostics(async () => result)).toBe(result);
    expect(output).not.toHaveBeenCalled();
  });
});
