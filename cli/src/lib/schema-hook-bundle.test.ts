// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

const require = createRequire(import.meta.url);
const dependencies = dirname(dirname(require.resolve("typescript/package.json")));
let root: string;
let script: string;
let output: string;

beforeEach(() => {
  root = mkdtempSync(join(tmpdir(), "kars-schema-bundle-test-"));
  mkdirSync(join(root, "cli/scripts"), { recursive: true });
  mkdirSync(join(root, "cli/src"));
  mkdirSync(join(root, "deploy/helm/kars/files"), { recursive: true });
  symlinkSync(dependencies, join(root, "node_modules"), "dir");
  writeFileSync(join(root, "package.json"), JSON.stringify({ private: true, type: "module" }));
  writeFileSync(join(root, "cli/package-lock.json"), JSON.stringify({
    packages: { "node_modules/yaml": { version: require("yaml/package.json").version } },
  }));
  writeFileSync(join(root, "cli/src/schema-hook.ts"), 'import YAML from "yaml"; console.log(YAML.stringify({ ok: true }));\n');
  script = join(root, "cli/scripts/bundle-schema-hook.mjs");
  output = join(root, "deploy/helm/kars/files/schema-hook.mjs");
  copyFileSync(fileURLToPath(new URL("../../scripts/bundle-schema-hook.mjs", import.meta.url)), script);
});

afterEach(() => rmSync(root, { recursive: true, force: true }));

function run(...args: string[]) {
  const result = spawnSync(process.execPath, [script, ...args], { cwd: root, encoding: "utf8", timeout: 10000 });
  expect(result.error).toBeUndefined();
  expect(result.signal).toBeNull();
  expect(readdirSync(join(root, "cli")).filter(name => name.startsWith(".schema-hook-build-"))).toEqual([]);
  return result;
}

describe("standalone schema helper generation", () => {
  it("produces deterministic executable output and its dependency license", () => {
    expect(run().status).toBe(0);
    const before = readFileSync(output);
    expect(before.toString()).toMatch(/^\/\/ Copyright \(c\) Microsoft Corporation\.\n\/\/ Licensed under the MIT License\.\n/);
    expect(run("--check").status).toBe(0);
    expect(readFileSync(output)).toEqual(before);
    expect(readFileSync(join(root, "deploy/helm/kars/files/schema-hook.NOTICE"), "utf8")).toContain("Permission to use, copy, modify");
    const executed = spawnSync(process.execPath, [output], { encoding: "utf8", timeout: 5000 });
    expect(executed.status).toBe(0);
    expect(executed.stdout).toBe("ok: true\n\n");
  });

  it.each(["--check", "build"])("refuses mismatched yaml dependencies during %s without writing output", mode => {
    writeFileSync(join(root, "cli/package-lock.json"), JSON.stringify({
      packages: { "node_modules/yaml": { version: "0.0.0" } },
    }));
    const result = run(...(mode === "--check" ? [mode] : []));
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("Schema helper requires lockfile yaml 0.0.0");
    expect(readdirSync(dirname(output))).toEqual([]);
  });

  it("refuses missing yaml lock metadata without writing output", () => {
    writeFileSync(join(root, "cli/package-lock.json"), JSON.stringify({ packages: {} }));
    expect(run().status).toBe(1);
    expect(readdirSync(dirname(output))).toEqual([]);
  });

  it("refuses stale generated output without rewriting it", () => {
    expect(run().status).toBe(0);
    writeFileSync(output, "stale generated output\n");
    expect(run("--check").status).toBe(1);
    expect(readFileSync(output, "utf8")).toBe("stale generated output\n");
  });

  it("refuses a missing generated output without recreating it", () => {
    expect(run("--check").status).toBe(1);
    expect(readdirSync(dirname(output))).toEqual([]);
  });

  it("rejects unsupported arguments before generating anything", () => {
    expect(run("--force").status).toBe(1);
    expect(readdirSync(dirname(output))).toEqual([]);
  });
});
