// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Shared helpers for the `up/` and `headlamp` command families.
 *
 * Extracted from `cli/src/commands/dev/local-k8s.ts` so the same
 * helpers can be reused by `kars headlamp --install` on AKS without
 * dragging in the kind-specific bring-up surface.
 */

import { existsSync } from "node:fs";
import * as path from "node:path";

/**
 * Walk up from `start` looking for the repo's `Cargo.toml` marker.
 * Used to locate sibling source trees (e.g. `tools/headlamp-plugin/`,
 * `deploy/monitoring/`) regardless of where the user invokes `kars`
 * from.
 *
 * Throws if no `Cargo.toml` is found between `start` and `/` — this
 * is a hard error because every code path that needs `findRepoRoot`
 * also needs the on-disk artifacts.
 */
export function findRepoRoot(start: string): string {
  let cur = start;
  while (cur !== "/" && !existsSync(path.join(cur, "Cargo.toml"))) {
    cur = path.dirname(cur);
  }
  if (cur === "/") {
    throw new Error(
      "Could not locate repo root (Cargo.toml). Run from inside the kars checkout.",
    );
  }
  return cur;
}
