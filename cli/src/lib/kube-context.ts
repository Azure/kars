// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * resolveKubeContext — auto-discover a reachable kubectl context.
 *
 * Several kars commands (`kars list`, `kars connect`, `kars operator`)
 * need to call kubectl but should not require the user to have an
 * active current-context set. Without auto-discovery, kubectl falls
 * through to `http://localhost:8080` and every query silently fails.
 *
 * Resolution order:
 *   1. If the caller passes an explicit context, return it verbatim.
 *   2. Else, try `kubectl config current-context`. Return it if set.
 *   3. Else, list every kubeconfig context and probe each with a 3s
 *      `kubectl get ns` budget. Return the first reachable one.
 *   4. Else, return undefined — caller should surface a clear error
 *      rather than fall through to localhost:8080.
 */

import { execa } from "execa";

export async function resolveKubeContext(explicit?: string): Promise<string | undefined> {
  if (explicit) return explicit;
  try {
    const { stdout } = await execa("kubectl", ["config", "current-context"], { stdio: "pipe" });
    if (stdout.trim()) return stdout.trim();
  } catch { /* no current context — probe the list */ }
  try {
    const { stdout } = await execa("kubectl", ["config", "get-contexts", "-o", "name"], { stdio: "pipe" });
    const candidates = stdout.trim().split("\n").filter(Boolean);
    for (const ctx of candidates) {
      try {
        await execa("kubectl", ["--context", ctx, "get", "ns", "--request-timeout=3s", "--no-headers"], { stdio: "pipe", timeout: 5000 });
        return ctx;
      } catch { /* try next */ }
    }
  } catch { /* no contexts at all */ }
  return undefined;
}
