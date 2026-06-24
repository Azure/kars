// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * repo-assets — resolve repo-relative deploy/build assets so the CLI works
 * BOTH from a kars repo checkout AND from an npm-installed package.
 *
 * `kars dev --release` / `kars up --release` are the no-compile, no-checkout
 * paths: a user runs `npm i -g @kars-runtime/cli` and expects everything to
 * work out of the box. But the K8s flows need static assets that live in the
 * repo — the Helm chart (`deploy/helm/kars`), the AgentMesh manifest
 * (`deploy/agentmesh-agt.yaml`), monitoring manifests, etc. Those are
 * **bundled into the published package** (copied into `dist/deploy/…` at build
 * time; see cli/package.json `build`), so this resolver checks, in order:
 *
 *   1. A kars repo checkout, by walking up from `process.cwd()`.
 *   2. The assets bundled alongside the compiled CLI (`<dist>/<relPath>`).
 *
 * Returns an absolute path, or null when the asset is found in neither place.
 */

import { existsSync } from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

/** Marker files that identify a kars repo checkout root. */
const REPO_MARKERS = ["Cargo.toml", "deploy/helm/kars"];

/** Directory containing the compiled CLI bundle (`dist/`). */
function bundleRoot(): string {
  // This module compiles to `dist/lib/repo-assets.js`, so `dist/` is one up.
  const here = path.dirname(fileURLToPath(import.meta.url));
  return path.resolve(here, "..");
}

/** Find the nearest kars repo checkout root above `start`, or null. */
export function findRepoRootOrNull(start: string = process.cwd()): string | null {
  let dir = path.resolve(start);
  for (let i = 0; i < 24; i++) {
    if (REPO_MARKERS.some((m) => existsSync(path.join(dir, m)))) return dir;
    const parent = path.dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }
  return null;
}

/**
 * Resolve a repo-relative asset path (e.g. "deploy/helm/kars",
 * "deploy/agentmesh-agt.yaml") to an absolute path that exists, checking a
 * repo checkout first, then the bundled copy shipped with the package.
 * Returns null if found in neither location.
 */
export function resolveBundledAsset(relPath: string, cwd: string = process.cwd()): string | null {
  const repoRoot = findRepoRootOrNull(cwd);
  if (repoRoot) {
    const inRepo = path.join(repoRoot, relPath);
    if (existsSync(inRepo)) return inRepo;
  }
  const bundled = path.join(bundleRoot(), relPath);
  if (existsSync(bundled)) return bundled;
  return null;
}

/**
 * Like {@link resolveBundledAsset} but throws a clear, actionable error when
 * the asset cannot be found in a repo checkout or the bundled package.
 */
export function requireBundledAsset(relPath: string, cwd: string = process.cwd()): string {
  const resolved = resolveBundledAsset(relPath, cwd);
  if (resolved) return resolved;
  throw new Error(
    `Could not locate '${relPath}'.\n` +
      `  It should be bundled with the kars CLI (npm package) or present in a kars\n` +
      `  repo checkout. Try reinstalling the CLI:\n` +
      `      npm i -g @kars-runtime/cli\n` +
      `  or run the command from inside a kars repo checkout.`,
  );
}
