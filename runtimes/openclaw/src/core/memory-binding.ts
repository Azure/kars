// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Memory-binding reader — resolves the canonical KarsMemory store name (and
 * default scope) for this sandbox from the mounted ConfigMap at
 *   /etc/kars/memory/binding.json
 *
 * The ConfigMap is projected by the kars controller when a KarsSandbox
 * has `spec.memoryRef.name` set. Its shape mirrors `LoadedMemoryBinding` on
 * the router side (see inference-router/src/mcp/platform.rs):
 *   {
 *     "storeName": "memory-<sandbox>",
 *     "scope": "agent:<sandbox>",
 *     "retentionDays": 30,
 *     ...
 *   }
 *
 * When the file is missing (legacy sandbox without KarsMemory CR, or
 * operator hasn't wired memoryRef yet) we fall back to the historical
 * plugin convention `memory-${agentName}` so existing sandboxes keep
 * working unchanged.
 *
 * The value is read once and cached for the process lifetime; the router
 * watches the file for changes, but the plugin re-reads on each new
 * sandbox boot which is sufficient for the policy semantics we promise
 * (binding edits become effective on the next pod roll).
 */

import { existsSync, readFileSync } from "node:fs";

const BINDING_PATH =
  process.env.KARS_MEMORY_BINDING_PATH ||
  "/etc/kars/memory/binding.json";

interface MemoryBinding {
  storeName?: string;
  store_name?: string;
  scope?: string;
  retentionDays?: number;
  retention_days?: number;
}

let cached: MemoryBinding | null | undefined;

function readBinding(): MemoryBinding | null {
  if (cached !== undefined) return cached;
  try {
    if (!existsSync(BINDING_PATH)) {
      cached = null;
      return null;
    }
    const raw = readFileSync(BINDING_PATH, "utf8");
    cached = JSON.parse(raw) as MemoryBinding;
    return cached;
  } catch {
    cached = null;
    return null;
  }
}

/** Resolve the memory store name for this sandbox. */
export function resolveMemoryStoreName(agentName: string): string {
  const b = readBinding();
  const name = b?.storeName || b?.store_name;
  if (typeof name === "string" && name.trim().length > 0) return name;
  return `memory-${agentName}`;
}

/** Resolve the default memory scope for this sandbox. */
export function resolveMemoryScope(agentName: string): string {
  const b = readBinding();
  const scope = b?.scope;
  if (typeof scope === "string" && scope.trim().length > 0) return scope;
  const cluster = (process.env.CLUSTER_NAME || "").trim();
  if (cluster.length > 0) return `agent:${cluster}/${agentName}`;
  return `agent:${agentName}`;
}

/** Test hook — reset the binding cache. */
export function __resetMemoryBindingCacheForTests(): void {
  cached = undefined;
}
