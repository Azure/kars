// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { runSchemaHook } from "./schema-hook.js";
import { schemaFixture } from "./lib/schema-stage.test-support.js";

const args = ["--release", "kars", "--namespace", "kars-system", "--revision", "1",
  "--operation", "install", "--installer-helm-version", "v4.1.3", "--timeout-ms", "1500"];

describe("schema hook command", () => {
  it("executes the real pending-release orchestration with exact parsed arguments", async () => {
    const f = schemaFixture();
    f.history[0].status = "pending-install";
    const result = await runSchemaHook(args, f.execute);
    expect(result).toMatchObject({ schemas: 1, published: true, revision: 1 });
    expect(f.writes).toHaveLength(1);
    expect(f.requests.filter(item => item.file === "helm" && item.args[0] === "get")
      .every(item => item.args.includes("--revision"))).toBe(true);
  });

  it.each([
    [], ["--force", "true"], [...args, "--release", "another"], [...args, "--take-ownership", "true"],
    [...args, "--extra"], args.slice(0, 8), ["--release"], ["--release", "--namespace"],
    args.map(item => item === "1500" ? "1e3" : item), args.map(item => item === "1" ? "1.0" : item),
    args.map(item => item === "install" ? "rollback" : item), args.map(item => item === "v4.1.3" ? "v3.19.0" : item),
  ].map(args => ({ args })))("refuses ambiguous or unsupported arguments $args before executing anything", async ({ args: invalid }) => {
    const execute = vi.fn();
    await expect(runSchemaHook(invalid, execute)).rejects.toThrow();
    expect(execute).not.toHaveBeenCalled();
  });
});
