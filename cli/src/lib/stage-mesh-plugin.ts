// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * stage-mesh-plugin — ensures `mesh-plugin/dist/` exists before any
 * code path runs `docker build` against `sandbox-images/openclaw/Dockerfile`.
 *
 * The sandbox Dockerfile (lines 36 + 70) hard-COPYs `mesh-plugin/dist/`
 * from the build context — but `dist/` is `.gitignore`d, so a fresh
 * clone has no dist and the build fails with:
 *
 *   ERROR: COPY mesh-plugin/dist/ … "/mesh-plugin/dist": not found
 *
 * CI builds it in a dedicated `mesh-plugin-build` job (see
 * `.github/workflows/ci.yml`); locally we need to do the same before
 * shelling out to docker.
 *
 * Idempotent: skips when `dist/` is already up to date relative to
 * `src/` and `package.json`. Re-runs when `forceRebuild` is true.
 */

import { execa } from "execa";
import * as path from "node:path";
import { existsSync, statSync, readdirSync } from "node:fs";

export interface StageMeshPluginOpts {
  /** Force `npm run build` even when dist/ looks fresh. */
  forceRebuild?: boolean;
}

/** Recursive mtime of every file under `dir`. */
function newestMtime(dir: string): number {
  let newest = 0;
  if (!existsSync(dir)) return 0;
  const walk = (d: string) => {
    for (const e of readdirSync(d, { withFileTypes: true })) {
      const p = path.join(d, e.name);
      if (e.isDirectory()) walk(p);
      else if (e.isFile()) newest = Math.max(newest, statSync(p).mtimeMs);
    }
  };
  walk(dir);
  return newest;
}

export async function stageMeshPlugin(
  repoRoot: string,
  opts: StageMeshPluginOpts = {},
): Promise<void> {
  const pluginDir = path.join(repoRoot, "mesh-plugin");
  const distDir = path.join(pluginDir, "dist");
  const srcDir = path.join(pluginDir, "src");
  const pkgJson = path.join(pluginDir, "package.json");

  if (!existsSync(pluginDir)) {
    throw new Error(`mesh-plugin directory not found at ${pluginDir}`);
  }

  if (!opts.forceRebuild && existsSync(path.join(distDir, "index.js"))) {
    const distMtime = newestMtime(distDir);
    const srcMtime = Math.max(newestMtime(srcDir), statSync(pkgJson).mtimeMs);
    if (distMtime >= srcMtime) return;
  }

  // npm ci is faster + deterministic when lockfile + node_modules already
  // match; fall back to npm install when node_modules is missing/stale.
  const hasNodeModules = existsSync(path.join(pluginDir, "node_modules"));
  const installCmd = hasNodeModules ? "ci" : "install";
  await execa("npm", [installCmd, "--prefer-offline", "--no-audit", "--no-fund"], {
    cwd: pluginDir,
    stdio: "inherit",
  });
  await execa("npm", ["run", "build"], { cwd: pluginDir, stdio: "inherit" });
}
