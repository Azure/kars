// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect, afterEach } from "vitest";
import { existsSync, mkdtempSync, mkdirSync, writeFileSync, cpSync, symlinkSync, rmSync } from "node:fs";
import { execFileSync } from "node:child_process";
import * as os from "node:os";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import {
  findRepoRootOrNull,
  resolveBundledAsset,
  requireBundledAsset,
} from "./repo-assets.js";

// Repo root, derived from this test file's location (cli/src/lib/ → ../../..).
const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "..", "..", "..");

/**
 * The deploy/build assets each `--release` use case needs to run with NO
 * repo checkout (npm-installed CLI). These MUST be:
 *   1. present at the repo root (so the build can bundle them), and
 *   2. listed in cli/scripts/bundle-deploy-assets.mjs (so they ARE bundled).
 *
 * This list is the regression guard for the class of bug where a `--release`
 * path silently assumes a repo checkout — exactly what broke
 * `kars dev --release --target local-k8s` out of the box.
 */
const REQUIRED_RELEASE_ASSETS = [
  // local-k8s (kars dev --release --target local-k8s)
  "deploy/helm/kars",
  "deploy/helm/kars/values-local-dev.yaml",
  "deploy/helm/kars/values-generic.yaml",
  "deploy/helm/kars/values-existing-aks.yaml",
  "deploy/agentmesh-agt.yaml",
  // aks (kars up --release)
  "deploy/bicep/main.bicep",
  // shared / observability (best-effort)
  "deploy/agentmesh-ingress.yaml",
  "deploy/monitoring",
  "tools/headlamp-plugin/dist/main.js",
  "tools/headlamp-plugin/package.json",
];

describe("findRepoRootOrNull", () => {
  it("finds the kars repo root from inside the checkout", () => {
    const root = findRepoRootOrNull(here);
    expect(root).toBe(repoRoot);
  });

  it("returns null when run from outside any repo (e.g. an npm global dir)", () => {
    const tmp = mkdtempSync(path.join(os.tmpdir(), "kars-norepo-"));
    expect(findRepoRootOrNull(tmp)).toBeNull();
  });
});

describe("resolveBundledAsset", () => {
  it("resolves every required release asset from a repo checkout", () => {
    for (const rel of REQUIRED_RELEASE_ASSETS) {
      const resolved = resolveBundledAsset(rel, here);
      expect(resolved, `asset '${rel}' must resolve`).not.toBeNull();
      expect(existsSync(resolved!), `'${rel}' resolved to a real path`).toBe(true);
    }
  });

  it("returns null for an asset that exists nowhere", () => {
    expect(resolveBundledAsset("deploy/does-not-exist.yaml", here)).toBeNull();
  });
});

describe("requireBundledAsset", () => {
  it("throws an actionable error for a missing asset", () => {
    expect(() => requireBundledAsset("deploy/nope.yaml", here)).toThrow(
      /Could not locate 'deploy\/nope\.yaml'/,
    );
  });
});

/**
 * Guards the bundle manifest itself: every required release asset must be
 * declared in the build's bundling script, or a future asset addition would
 * silently not ship and break `--release` OOTB again.
 */
describe("bundle-deploy-assets manifest", () => {
  it("declares (a prefix of) every required release asset", async () => {
    const { readFileSync } = await import("node:fs");
    const script = readFileSync(
      path.join(repoRoot, "cli", "scripts", "bundle-deploy-assets.mjs"),
      "utf8",
    );
    // Extract the quoted asset paths the script declares in its `assets` list.
    const declared = [...script.matchAll(/"((?:deploy|tools)\/[^"]+)"/g)].map((m) => m[1]);
    expect(declared.length).toBeGreaterThan(0);
    for (const rel of REQUIRED_RELEASE_ASSETS) {
      // Covered if the asset is declared directly, or lives under a declared
      // directory entry (the script copies directories recursively).
      const covered = declared.some((d) => rel === d || rel.startsWith(d + "/"));
      expect(covered, `bundle script must cover '${rel}'`).toBe(true);
    }
  });
});

describe("locked Core chart bundling", () => {
  const fixtures: string[] = [];
  afterEach(() => {
    for (const root of fixtures.splice(0)) rmSync(root, { recursive: true, force: true });
  });

  function fixture() {
    const root = mkdtempSync(path.join(os.tmpdir(), "kars-bundle-chart-"));
    fixtures.push(root);
    const script = path.join(root, "cli/scripts/bundle-deploy-assets.mjs");
    const chart = path.join(root, "deploy/helm/kars");
    const distChart = path.join(root, "cli/dist/deploy/helm/kars");
    mkdirSync(path.dirname(script), { recursive: true });
    mkdirSync(path.join(chart, "charts"), { recursive: true });
    mkdirSync(distChart, { recursive: true });
    cpSync(path.join(repoRoot, "cli/scripts/bundle-deploy-assets.mjs"), script);
    symlinkSync(path.join(repoRoot, "cli/node_modules"), path.join(root, "cli/node_modules"), "dir");
    const dependencies = [
      { name: "dependency", alias: "monitoring", version: "1.2.3", repository: "https://charts.example.test" },
      { name: "local-child", alias: "datapathWitness", version: "0.1.0", repository: "file://../local-child" },
    ];
    writeFileSync(path.join(chart, "Chart.yaml"), JSON.stringify({ apiVersion: "v2", name: "kars", dependencies }));
    writeFileSync(path.join(chart, "Chart.lock"), JSON.stringify({
      dependencies: dependencies.map(({ alias: _alias, ...dependency }) => dependency),
    }));
    for (const dependency of dependencies) {
      writeFileSync(path.join(chart, "charts", `${dependency.name}-${dependency.version}.tgz`), "fixture archive");
    }
    writeFileSync(path.join(root, "deploy/agentmesh-agt.yaml"), "kind: List\nitems: []\n");
    writeFileSync(path.join(distChart, "obsolete-template.yaml"), "stale");
    return {
      chart, distChart,
      bundle: () => execFileSync(process.execPath, [script], { encoding: "utf8", stdio: "pipe" }),
    };
  }

  it("bundles remote and local locked dependencies by original name and removes stale chart files", () => {
    const { distChart, bundle } = fixture();
    expect(bundle()).toContain("copied");
    expect(existsSync(path.join(distChart, "charts/dependency-1.2.3.tgz"))).toBe(true);
    expect(existsSync(path.join(distChart, "charts/local-child-0.1.0.tgz"))).toBe(true);
    expect(existsSync(path.join(distChart, "obsolete-template.yaml"))).toBe(false);
  });

  it.each(["Chart.lock", "charts/dependency-1.2.3.tgz", "charts/local-child-0.1.0.tgz"])(
    "fails before replacing the bundle when %s is missing",
    (missing) => {
      const { chart, distChart, bundle } = fixture();
      rmSync(path.join(chart, missing));
      expect(bundle).toThrow(/helm dependency build deploy\/helm\/kars/);
      expect(existsSync(path.join(distChart, "obsolete-template.yaml"))).toBe(true);
    },
  );

  it.each(["stale", "duplicate", "malformed"])("refuses a %s dependency lock", (kind) => {
    const { chart, bundle } = fixture();
    const dependency = { name: "dependency", version: "1.2.2", repository: "https://charts.example.test" };
    const dependencies = kind === "stale" ? [dependency]
      : kind === "duplicate" ? [dependency, dependency] : "not-an-array";
    writeFileSync(path.join(chart, "Chart.lock"), JSON.stringify({ dependencies }));
    expect(bundle).toThrow(/match Chart.lock/);
  });

  it.each(["empty", "directory", "symlink"])("refuses a %s dependency archive", (kind) => {
    const { chart, bundle } = fixture();
    const archive = path.join(chart, "charts/dependency-1.2.3.tgz");
    rmSync(archive);
    if (kind === "empty") writeFileSync(archive, "");
    else if (kind === "directory") mkdirSync(archive);
    else symlinkSync(path.join(chart, "Chart.yaml"), archive);
    expect(bundle).toThrow(/not a regular archive/);
  });
});
