// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import { existsSync, mkdtempSync } from "node:fs";
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
