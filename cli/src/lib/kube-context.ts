// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * resolveKubeContext — pick a kubectl context for read-only multi-cluster
 * discovery flows (e.g. `kars list`, `kars operator`).
 *
 * Resolution order:
 *   1. Explicit caller-provided context — return verbatim.
 *   2. `$KARS_KUBE_CONTEXT` — kars-scoped explicit pick.
 *   3. `kubectl config current-context` — what the user already chose
 *      (or what tools like `az aks get-credentials --overwrite-existing`
 *      set automatically).
 *   4. undefined — caller decides whether to error or fall through.
 *
 * Note: this function deliberately does NOT auto-discover a reachable
 * context by probing every kubeconfig entry. Doing so blindly is
 * dangerous for write commands (a user with prod + dev kubeconfigs
 * would have no way to notice that `kars push --apply` targeted prod).
 * Write commands should go through `bootstrapKubeContext` which fails
 * loud with an actionable error instead of guessing.
 */

import { execa } from "execa";

export async function resolveKubeContext(explicit?: string): Promise<string | undefined> {
  if (explicit) return explicit;
  const envPick = process.env.KARS_KUBE_CONTEXT?.trim();
  if (envPick) return envPick;
  try {
    const { stdout } = await execa("kubectl", ["config", "current-context"], { stdio: "pipe" });
    if (stdout.trim()) return stdout.trim();
  } catch { /* no current context */ }
  return undefined;
}
