// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import { parse } from "yaml";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const auditScript = join(root, "ci/npm-audit-bulk.mjs");
const temporaryDirectories: string[] = [];
const advisory = {
  id: 1234,
  name: "example",
  severity: "high",
  title: "Advisory fixture",
  url: "https://github.com/advisories/GHSA-example",
  vulnerable_versions: "<2.0.0",
};
const lockfile = {
  lockfileVersion: 3,
  packages: {
    "": { name: "fixture" },
    "node_modules/example": { version: "1.0.0" },
  },
};

// Exercise the real CLI entry point with an isolated registry, without network access.
const runner = `
  import { pathToFileURL } from "node:url";
  const steps = JSON.parse(process.env.AUDIT_STEPS);
  let calls = 0;
  globalThis.setTimeout = (callback) => { callback(); return 0; };
  globalThis.fetch = async (url, options) => {
    if (url !== "https://registry.npmjs.org/-/npm/v1/security/advisories/bulk") {
      throw new Error("unexpected registry");
    }
    console.log("PAYLOAD:" + options.body);
    const step = steps[Math.min(calls++, steps.length - 1)];
    if (step.transportError) throw new TypeError("registry unreachable");
    return new Response(step.raw ?? JSON.stringify(step.body), {
      status: step.status ?? 200,
      headers: { "content-type": "application/json" },
    });
  };
  process.on("exit", () => console.log("CALLS:" + calls));
  process.argv = [process.execPath, process.env.AUDIT_SCRIPT, process.env.AUDIT_LOCK];
  await import(pathToFileURL(process.env.AUDIT_SCRIPT).href);
`;

function audit(steps: object[], lock: unknown = lockfile) {
  const directory = mkdtempSync(join(tmpdir(), "kars-audit-test-"));
  temporaryDirectories.push(directory);
  const lockPath = join(directory, "package-lock.json");
  writeFileSync(lockPath, JSON.stringify(lock));
  const result = spawnSync(process.execPath, ["--input-type=module", "-e", runner], {
    encoding: "utf8",
    timeout: 10_000,
    env: {
      ...process.env,
      AUDIT_SCRIPT: auditScript,
      AUDIT_LOCK: lockPath,
      AUDIT_STEPS: JSON.stringify(steps),
    },
  });
  if (result.error) throw result.error;
  expect(result.signal).toBeNull();
  return result;
}

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0)) {
    rmSync(directory, { recursive: true, force: true });
  }
});

describe("npm audit gate", () => {
  it.each(["cli", "mesh-plugin", "runtimes/openclaw"])("audits the complete %s lockfile", (project) => {
    const lock = JSON.parse(readFileSync(join(root, project, "package-lock.json"), "utf8"));
    const result = audit([{ body: {} }], lock);
    expect(result.status).toBe(0);
    expect(result.stdout).toContain("CALLS:1");
    const line = result.stdout.split("\n").find((entry) => entry.startsWith("PAYLOAD:"));
    expect(line).toBeDefined();
    expect(Object.keys(JSON.parse(line!.slice("PAYLOAD:".length))).length).toBeGreaterThan(0);
  });

  it("accepts a valid empty advisory map after auditing packages", () => {
    const result = audit([{ body: {} }]);
    expect(result.status).toBe(0);
    expect(result.stdout).toContain('PAYLOAD:{"example":["1.0.0"]}');
    expect(result.stdout).toContain("1 packages, 0 advisories, 0 blocking");
  });

  it.each(["high", "critical"])("blocks %s advisories", (severity) => {
    const result = audit([{ body: { example: [{ ...advisory, severity }] } }]);
    expect(result.status).toBe(1);
    expect(result.stdout).toContain("1 blocking");
  });

  it.each(["info", "low", "moderate", "high", "critical"])(
    "handles standard npm bulk %s records whose package name is only in the map key",
    (severity) => {
      const result = audit([{ body: { example: [{ ...advisory, name: undefined, severity }] } }]);
      expect(result.status).toBe(["high", "critical"].includes(severity) ? 1 : 0);
      expect(result.stdout).toContain(`${severity}: example`);
      expect(result.stderr).not.toContain("Invalid npm advisory");
    },
  );

  it.each(["info", "low", "moderate"])("reports nonblocking %s advisories", (severity) => {
    const result = audit([{ body: { example: [{ ...advisory, severity }] } }]);
    expect(result.status).toBe(0);
    expect(result.stdout).toContain(`${severity}: example`);
  });

  it.each([
    null, [], "error", { error: "service unavailable" },
    { example: advisory }, { example: [null] },
    { example: [{ ...advisory, severity: "unknown" }] },
    { example: [{ ...advisory, name: "another-package" }] },
    { example: [{ ...advisory, name: null }] },
    { example: [{ ...advisory, id: undefined }] },
    { example: [{ ...advisory, vulnerable_versions: "" }] },
    { unknown: [] },
  ])("rejects malformed success responses: %j", (body) => {
    const result = audit([{ body }]);
    expect(result.status).not.toBe(0);
    expect(result.stderr).toContain("Invalid npm advisory");
    expect(result.stdout).not.toContain("0 blocking");
  });

  it("rejects invalid JSON", () => {
    expect(audit([{ raw: "<html>unavailable</html>" }]).status).not.toBe(0);
  });

  it.each([
    { transportError: true }, { status: 429 }, { status: 503 },
  ])("retries transient errors before processing the result: %j", (failure) => {
    const result = audit([failure, { body: { example: [advisory] } }]);
    expect(result.status).toBe(1);
    expect(result.stdout).toContain("CALLS:2");
    expect(result.stdout).toContain("1 blocking");
  });

  it("recovers from a transient error with a valid empty map", () => {
    const result = audit([{ status: 503 }, { body: {} }]);
    expect(result.status).toBe(0);
    expect(result.stdout).toContain("CALLS:2");
  });

  it.each([{ transportError: true }, { status: 503 }])("fails after four retries: %j", (step) => {
    const result = audit([step]);
    expect(result.status).not.toBe(0);
    expect(result.stdout).toContain("CALLS:4");
    expect(result.stdout).not.toContain("0 blocking");
  });

  it("reports the final transport error rather than an earlier HTTP response", () => {
    const result = audit([{ status: 503 }, { transportError: true }]);
    expect(result.status).not.toBe(0);
    expect(result.stderr).toContain("failed after four attempts");
    expect(result.stdout).toContain("CALLS:4");
  });

  it("does not retry a non-transient HTTP error", () => {
    const result = audit([{ status: 400 }]);
    expect(result.status).not.toBe(0);
    expect(result.stdout).toContain("CALLS:1");
  });

  it.each([
    null, {}, { lockfileVersion: 1, dependencies: {} },
    { lockfileVersion: 3, packages: [] },
    { lockfileVersion: 3, packages: {} },
    { lockfileVersion: 3, packages: { "node_modules/example": {} } },
  ])("refuses an incomplete audit payload: %j", (lock) => {
    const result = audit([{ body: {} }], lock);
    expect(result.status).not.toBe(0);
    expect(result.stdout).toContain("CALLS:0");
  });

  it("uses real names for aliases and deduplicates nested and scoped versions", () => {
    const result = audit([{ body: {} }], {
      lockfileVersion: 2,
      packages: {
        "": {},
        "node_modules/alias": { name: "example", version: "1.0.0" },
        "node_modules/example": { version: "1.0.0" },
        "node_modules/parent/node_modules/example": { version: "1.1.0" },
        "node_modules/@scope/name": { version: "2.0.0" },
        "node_modules/linked": { link: true, resolved: "../linked" },
      },
    });
    expect(result.status).toBe(0);
    expect(result.stdout).toContain('"example":["1.0.0","1.1.0"]');
    expect(result.stdout).toContain('"@scope/name":["2.0.0"]');
    expect(result.stdout).not.toContain('"linked"');
  });
});

describe("stacked publication gates", () => {
  it.each(["ci", "ci-gates", "codeql", "dependency-review", "secret-scanning"])(
    "%s retains main and covers publication PR bases without privileged PR execution",
    (name) => {
      const workflow = parse(readFileSync(join(root, `.github/workflows/${name}.yml`), "utf8"));
      expect(workflow.on.pull_request.branches).toContain("main");
      expect(workflow.on.pull_request.branches).toContain("public/pr*");
      expect(workflow.on.pull_request_target).toBeUndefined();
    },
  );

  it("keeps all three npm audits mandatory", () => {
    const workflow = parse(readFileSync(join(root, ".github/workflows/ci.yml"), "utf8"));
    for (const jobName of ["cli-build", "runtime-openclaw-build", "mesh-plugin-build"]) {
      const job = workflow.jobs[jobName];
      expect(job["continue-on-error"]).not.toBe(true);
      const steps = job.steps.filter((step: { run?: string }) => step.run?.includes("npm-audit-bulk.mjs"));
      expect(steps).toHaveLength(1);
      expect(steps[0]["continue-on-error"]).not.toBe(true);
      expect(steps[0].if).toBeUndefined();
      expect(steps[0].run).toMatch(/^node (?:\.\.\/){1,2}ci\/npm-audit-bulk\.mjs package-lock\.json$/);
    }
  });
});
