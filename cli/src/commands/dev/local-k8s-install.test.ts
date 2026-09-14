// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import { parseAllDocuments } from "yaml";
import { ensureCluster, installLocalCoreChart } from "./local-k8s.js";
import { schemaFixture } from "../../lib/schema-stage.test-support.js";
import type { SchemaExecute } from "../../lib/schema-documents.js";

const { execute } = vi.hoisted(() => ({ execute: vi.fn<SchemaExecute>() }));
vi.mock("execa", () => ({ execa: execute }));
afterEach(() => vi.restoreAllMocks());

describe("existing kind core installation target", () => {
  it("pins rendering, schema preparation and final apply despite a different ambient context", async () => {
    const f = schemaFixture();
    const globalConfig = { currentContext: "production-ambient" };
    const before = structuredClone(globalConfig);
    const finalApply: string[][] = [];
    execute.mockImplementation(async (file, args, options) => {
      if (file === "kind") {
        expect(args).toEqual(["get", "clusters"]);
        return { stdout: "chosen\nother\n" };
      }
      expect(args).not.toContain("config");
      const contextFlag = file === "/tools/helm" ? "--kube-context" : "--context";
      const index = args.indexOf(contextFlag);
      expect(index).toBeGreaterThanOrEqual(0);
      expect(args[index + 1]).toBe("kind-chosen");
      const command = args.filter((_, position) => position !== index && position !== index + 1);
      if (file === "/tools/kubectl" && command[0] === "apply" && options?.input) {
        const objects = parseAllDocuments(options.input).map(document => document.toJSON());
        if (objects.some(object => object.kind !== "CustomResourceDefinition")) {
          expect(objects.every(object => object.kind !== "CustomResourceDefinition")).toBe(true);
          finalApply.push([...args]);
          return { stdout: "configured" };
        }
      }
      return f.execute(file === "/tools/helm" ? "helm" : "kubectl", command, options ?? { stdio: "pipe" });
    });
    await ensureCluster("kind", "chosen", {});
    await installLocalCoreChart("/tools/helm", "/tools/kubectl", "chosen", "kars", "/exact/chart", ["/exact/values.yaml"]);
    expect(finalApply).toHaveLength(1);
    expect(f.writes).toHaveLength(1);
    expect(execute.mock.calls.some(([file, args]) => file === "kind" && args[0] === "create")).toBe(false);
    expect(globalConfig).toEqual(before);
    expect(f.requests.filter(request => request.args[0] === "template").map(request =>
      request.args.includes("--dry-run=server"))).toEqual([false]);
  });
});
