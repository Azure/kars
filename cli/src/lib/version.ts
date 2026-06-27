// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// lib/version.ts — the CLI's own published version, read from package.json.
// The release workflow keeps `cli/package.json` version == the release tag
// (e.g. 0.1.20 ↔ v0.1.20), so the CLI version is the authoritative marker
// of "which kars release this CLI deploys". `kars up` / `kars up --upgrade`
// stamp this into the Helm `karsRelease` value so `kars upgrade` can read
// back the deployed version (the chart's static appVersion can't be trusted).

import { createRequire } from "node:module";

const require = createRequire(import.meta.url);

let cached: string | null = null;

/** The CLI package version (e.g. "0.1.20"). Falls back to "0.0.0" if the
 *  package.json can't be located (should never happen in a packaged CLI). */
export function cliVersion(): string {
  if (cached) return cached;
  // dist/lib/version.js → ../../package.json == cli/package.json. Try a few
  // relative depths so this is robust to bundling layout changes.
  for (const rel of ["../../package.json", "../package.json", "../../../package.json"]) {
    try {
      const pkg = require(rel) as { version?: string };
      if (pkg?.version) {
        cached = pkg.version;
        return cached;
      }
    } catch {
      // try next
    }
  }
  cached = "0.0.0";
  return cached;
}

/** The CLI version as a release tag (always `v`-prefixed, e.g. "v0.1.20"). */
export function cliReleaseTag(): string {
  const v = cliVersion();
  return v.startsWith("v") ? v : `v${v}`;
}
