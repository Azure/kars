// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import { schemasCommand } from "./schemas.js";
import { admission, crd, schemaFixture } from "../lib/schema-stage.test-support.js";
import type { SchemaExecute } from "../lib/schema-documents.js";

const { execute } = vi.hoisted(() => ({ execute: vi.fn<SchemaExecute>() }));
vi.mock("execa", () => ({ execa: execute }));
afterEach(() => vi.restoreAllMocks());

describe("public operator schema preparation command", () => {
  it("runs the same lifecycle for direct Helm/native callers", async () => {
    const f = schemaFixture();
    execute.mockImplementation(f.execute);
    const output = vi.spyOn(console, "log").mockImplementation(() => {});
    await schemasCommand().parseAsync(["node", "schemas", "prepare", "--chart", "/exact/chart",
      "--release", "kars", "--namespace", "kars-system", "--context", "native", "--timeout", "30"]);
    expect(f.writes).toHaveLength(1);
    expect(f.writes[0].kind).toBe("CustomResourceDefinition");
    expect(f.requests.every(request => request.args.includes(request.file === "helm" ? "--kube-context" : "--context"))).toBe(true);
    expect(JSON.parse(String(output.mock.calls[0][0]))).toMatchObject({ published: true, schemas: 1 });
  });

  it("keeps check mode read-only and rejects an unprepared schema", async () => {
    const f = schemaFixture();
    execute.mockImplementation(f.execute);
    await expect(schemasCommand().parseAsync(["node", "schemas", "prepare", "--chart", "/exact/chart",
      "--release", "kars", "--namespace", "kars-system", "--check"])).rejects.toThrow("has not been staged");
    expect(f.writes).toEqual([]);
  });

  it("rejects invalid deadlines before executing any command", async () => {
    execute.mockClear();
    await expect(schemasCommand().parseAsync(["node", "schemas", "prepare",
      "--release", "kars", "--namespace", "kars-system", "--timeout", "0"])).rejects.toThrow("--timeout");
    expect(execute).not.toHaveBeenCalled();
  });

  it("requires atomic rollback retention for direct Helm/native callers before schema writes", async () => {
    const f = schemaFixture();
    const old = crd();
    delete old.metadata.annotations["helm.sh/resource-policy"];
    execute.mockImplementation((file, args, options) => file === "helm" && args[0] === "get" && args[1] === "manifest"
      ? Promise.resolve({ stdout: [old, admission()].map(object => JSON.stringify(object)).join("\n---\n") })
      : f.execute(file, args, options));
    await expect(schemasCommand().parseAsync(["node", "schemas", "prepare", "--chart", "/exact/chart",
      "--release", "kars", "--namespace", "kars-system", "--atomic"])).rejects.toThrow("retention migration");
    expect(f.writes).toEqual([]);
  });
});
