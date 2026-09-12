// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { prepareCoreHelmSchemas, prepareCoreRollbackSchemas, prepareCoreTemplateSchemas } from "./core-helm-schemas.js";
import { crd, admission, schemaFixture } from "./schema-stage.test-support.js";
import type { SchemaExecute } from "./schema-documents.js";
import { serverSchemaRenderFlags } from "./schema-helm-safety.js";

describe("shared core schema entrypoints", () => {
  it("prepares a fresh Helm installation before policy installation, with matching release/context and values", async () => {
    const f = schemaFixture();
    const args = ["upgrade", "--install", "kars", "/exact/chart", "--namespace", "kars-system",
      "--kube-context", "intended", "-f", "/exact/values.yaml", "--set-string", "provider=model", "--wait=legacy"];
    await prepareCoreHelmSchemas(f.execute, args);
    await f.execute("helm", args, { stdio: "pipe" });
    const helm = f.requests.at(-1)!;
    expect(helm.args).toEqual(args);
    expect(f.requests.slice(0, -1).every(request => request.args.includes(request.file === "helm" ? "--kube-context" : "--context"))).toBe(true);
    const render = f.requests.find(request => request.args[0] === "template")!;
    expect(render.args).toEqual(expect.arrayContaining(["/exact/chart", "-f", "/exact/values.yaml", "--set-string", "provider=model"]));
    const publication = f.requests.map(request => request.args.includes("/openapi/v3")).lastIndexOf(true);
    expect(publication).toBeLessThan(f.requests.length - 1);
    expect(f.writes.every(object => object.kind === "CustomResourceDefinition")).toBe(true);
  });

  it.each(["--reuse-values", "--reset-then-reuse-values"])("uses the appropriate saved values for %s without printing or replacing them", async flag => {
    const f = schemaFixture();
    await prepareCoreHelmSchemas(f.execute, ["upgrade", "kars", "chart", "--namespace", "kars-system", flag, "--set", "new=value"]);
    const values = f.requests.find(request => request.args[0] === "get" && request.args[1] === "values")!;
    expect(values.args.includes("--all")).toBe(flag === "--reuse-values");
    const render = f.requests.find(request => request.args[0] === "template")!;
    expect(JSON.parse(render.input!)).toEqual({ preserved: "saved" });
    expect(render.args.indexOf("-f")).toBeLessThan(render.args.indexOf("--set"));
  });

  it("treats only a successful empty release inventory as a fresh install", async () => {
    const f = schemaFixture();
    const run: SchemaExecute = (file, args, options) => file === "helm" && args[0] === "list"
      ? Promise.resolve({ stdout: "[]" }) : f.execute(file, args, options);
    await prepareCoreHelmSchemas(run, ["upgrade", "--install", "kars", "chart", "--namespace", "kars-system", "--reuse-values"]);
    expect(f.requests.some(request => request.args[0] === "get" && request.args[1] === "values")).toBe(false);
    const denied: SchemaExecute = async () => { throw new Error("release inventory forbidden"); };
    await expect(prepareCoreHelmSchemas(denied, ["upgrade", "kars", "chart", "-n", "kars-system", "--reuse-values"])).rejects.toThrow("forbidden");
  });

  it("does not return a template payload until its exact CRDs are established and published", async () => {
    const f = schemaFixture();
    const documents = [crd(), admission()];
    const remainder = await prepareCoreTemplateSchemas(f.execute, documents.map(object => JSON.stringify(object)).join("\n---\n"),
      { ...f.owner, ...f.wait, ownership: "template" });
    expect(JSON.parse(remainder)).toEqual(admission());
    expect(f.writes).toHaveLength(1);
    expect(f.writes[0].metadata.labels["app.kubernetes.io/managed-by"]).toBe("kars-schema-stage");
  });

  it.each(["v3.13.0", "v3.16.0", "v4.1.3"])("uses real server capabilities with supported Helm %s", async version => {
    const f = schemaFixture();
    f.options.helmVersion = version;
    const flags = await serverSchemaRenderFlags(f.execute);
    expect(flags).toContain("--dry-run=server");
    expect(flags.includes("--validate")).toBe(version.startsWith("v3."));
  });

  it.each(["v3.12.9", "v5.0.0", "invalid"])("rejects unsupported rendering semantics %s before writes", async version => {
    const f = schemaFixture();
    f.options.helmVersion = version;
    await expect(prepareCoreHelmSchemas(f.execute, ["upgrade", "kars", "chart", "-n", "kars-system"])).rejects.toThrow("server dry-run");
    expect(f.writes).toEqual([]);
  });

  it("does not accept different server-side CRDs after a fresh bootstrap plan", async () => {
    const f = schemaFixture();
    f.options.releaseExists = false;
    const execute: SchemaExecute = async (file, args, options) => {
      if (file === "helm" && args[0] === "template" && args.includes("--dry-run=server")) {
        const changed = crd();
        changed.spec.versions[0].schema.openAPIV3Schema.properties.spec.properties.fromLookup = { type: "string" };
        return { stdout: JSON.stringify(changed) };
      }
      return f.execute(file, args, options);
    };
    await expect(prepareCoreHelmSchemas(execute, ["upgrade", "--install", "kars", "chart", "-n", "kars-system", "--atomic"]))
      .rejects.toThrow("differ from the bootstrap");
    expect(f.writes).toHaveLength(1);
    expect(f.writes[0].spec).toEqual(crd().spec);
  });

  it("stages the exact previous Helm revision before returning an explicit rollback target", async () => {
    const f = schemaFixture();
    f.install(crd());
    const execute: SchemaExecute = (file, args, settings) => file === "helm" && args[0] === "history"
      ? Promise.resolve({ stdout: '[{"revision":1},{"revision":2}]' }) : f.execute(file, args, settings);
    await expect(prepareCoreRollbackSchemas(execute, "kars", "kars-system")).resolves.toBe(1);
    expect(f.requests.some(request => request.args.includes("--revision") && request.args.includes("1"))).toBe(true);
    expect(f.requests.some(request => request.args.includes("/openapi/v3"))).toBe(true);
  });

  it("refuses rollback that could delete a CRD before any schema mutation", async () => {
    const f = schemaFixture();
    const execute: SchemaExecute = (file, args, settings) => {
      if (file === "helm" && args[0] === "history") return Promise.resolve({ stdout: '[{"revision":1},{"revision":2}]' });
      if (file === "helm" && args.includes("--revision") && args.at(-1) === "1") {
        return Promise.resolve({ stdout: JSON.stringify(admission()) });
      }
      return f.execute(file, args, settings);
    };
    await expect(prepareCoreRollbackSchemas(execute, "kars", "kars-system")).rejects.toThrow("remove a core CRD");
    expect(f.writes).toEqual([]);
  });

  it.each([["--post-renderer", "/opaque/transform"], ["-f", "-"], ["--kube-token", "opaque"]])(
    "refuses unreviewable Helm input %s before any mutation", async (...extra) => {
      const f = schemaFixture();
      await expect(prepareCoreHelmSchemas(f.execute, ["install", "kars", "chart", "-n", "kars-system", ...extra])).rejects.toThrow();
      expect(f.writes).toEqual([]);
    });
});
