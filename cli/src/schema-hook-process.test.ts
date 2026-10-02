// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const entries = [
  { name: "compiled", entry: fileURLToPath(new URL("../dist/schema-hook.js", import.meta.url)) },
  { name: "bundled", entry: fileURLToPath(new URL("../../deploy/helm/kars/files/schema-hook.mjs", import.meta.url)) },
];
const args = ["--release", "kars", "--namespace", "kars-system", "--revision", "1",
  "--operation", "install", "--installer-helm-version", "v4.1.3"];

describe.each(entries)("$name schema hook process", ({ entry }) => {
  it("reports missing projected credentials without exposing filesystem paths or stack", () => {
    const result = spawnSync(process.execPath, [entry, ...args], { encoding: "utf8", timeout: 5000,
      env: { PATH: "/nonexistent", KUBERNETES_SERVICE_HOST: "127.0.0.1", KUBERNETES_SERVICE_PORT: "1" } });
    expect(result.error).toBeUndefined();
    expect(result.signal).toBeNull();
    expect(result.status).toBe(1);
    expect(result.stdout).toBe("");
    expect(result.stderr).toBe("Core schema staging failed. Helm must not proceed; no cluster cleanup, force or automatic write retry was attempted.\n");
  });

  it("refuses local-cluster fallback before starting Helm or kubectl", () => {
    const result = spawnSync(process.execPath, [entry, ...args], { encoding: "utf8", timeout: 5000,
      env: { PATH: "/nonexistent", HOME: "/nonexistent" } });
    expect(result.error).toBeUndefined();
    expect(result.signal).toBeNull();
    expect(result.status).toBe(1);
    expect(result.stdout).toBe("");
    expect(result.stderr).toMatch(/^Core schema staging failed\./);
    expect(result.stderr).not.toMatch(/ENOENT|spawn|stack|at /);
  });

  it("does not print rejected option values or inherited credential material", () => {
    const marker = "test-sensitive-value-must-not-be-logged";
    const result = spawnSync(process.execPath, [entry, ...args, "--token", marker], { encoding: "utf8", timeout: 5000,
      env: { PATH: "/nonexistent", PRIVATE_TEST_CREDENTIAL: marker } });
    expect(result.status).toBe(1);
    expect(result.stdout).toBe("");
    expect(result.stderr).toMatch(/^Core schema staging failed\./);
    expect(result.stderr).not.toContain(marker);
    expect(result.stderr).not.toContain("--token");
  });
});
