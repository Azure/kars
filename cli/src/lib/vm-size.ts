// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// VM-size (SKU) resolution for `kars up`.
//
// Azure subscriptions frequently restrict specific VM SKUs per region (trial,
// MSDN, sponsored, and many enterprise subscriptions individually gate sizes —
// e.g. the `_v5` D-series may be blocked while `_v4`/`_v6` of the same family are
// allowed). A single hardcoded default therefore cannot work out-of-the-box for
// every subscription, and the AKS system pool in particular previously pinned a
// size the caller could not override.
//
// This module queries the SKUs actually available to the caller's subscription
// in the target region and picks the first usable size from an ordered
// preference list (keeping the historical defaults first so behaviour is
// unchanged where they are available), or honours an explicit override.

import { execa } from "execa";

export interface SkuRestriction {
  type?: string;
  reasonCode?: string;
  restrictionInfo?: { locations?: string[]; zones?: string[] };
}

export interface VmSku {
  name?: string;
  resourceType?: string;
  restrictions?: SkuRestriction[];
}

// Ordered most-preferred → least. Historical defaults kept first so existing
// subscriptions get identical SKUs; the rest are same-class fallbacks (2 vCPU
// for the system pool, 4 vCPU for the sandbox/user pool).
export const SYSTEM_POOL_VM_PREFERENCES = [
  "Standard_D2as_v5",
  "Standard_D2s_v5",
  "Standard_D2as_v6",
  "Standard_D2s_v6",
  "Standard_D2as_v4",
  "Standard_D2s_v4",
  "Standard_D2as_v7",
  "Standard_D2s_v7",
  "Standard_D2as_v3",
  "Standard_D2s_v3",
];

export const USER_POOL_VM_PREFERENCES = [
  "Standard_D4s_v5",
  "Standard_D4as_v5",
  "Standard_D4s_v6",
  "Standard_D4as_v6",
  "Standard_D4s_v4",
  "Standard_D4as_v4",
  "Standard_D4s_v7",
  "Standard_D4as_v7",
  "Standard_D4s_v3",
  "Standard_D4as_v3",
];

/**
 * Build the set of SKU names (lower-cased) usable by the subscription in the
 * queried region. A SKU is unusable if it carries a `Location`-type restriction
 * (NotAvailableForSubscription in that region). Zone-only restrictions are
 * ignored — the kars pools are not zone-pinned.
 */
export function usableSkuSet(skus: VmSku[]): Set<string> {
  const usable = new Set<string>();
  for (const s of skus) {
    if (!s.name) continue;
    const blockedInRegion = (s.restrictions ?? []).some(
      (r) => (r.type ?? "").toLowerCase() === "location",
    );
    if (!blockedInRegion) usable.add(s.name.toLowerCase());
  }
  return usable;
}

/**
 * Pick a usable VM size. If `requested` is given it must be usable, otherwise we
 * throw with the usable subset of the preference list. With no request we return
 * the first usable preference.
 */
export function pickUsableVmSize(opts: {
  usable: Set<string>;
  preferences: string[];
  poolLabel: string;
  flagName: string;
  requested?: string;
}): string {
  const { usable, preferences, poolLabel, flagName, requested } = opts;

  if (requested) {
    if (usable.has(requested.toLowerCase())) return requested;
    const alts = preferences.filter((p) => usable.has(p.toLowerCase()));
    throw new Error(
      `Requested ${poolLabel} VM size '${requested}' is not available to your subscription in this region.` +
        (alts.length
          ? ` Available alternatives: ${alts.join(", ")}.`
          : ` Run \`az vm list-skus -l <region> --resource-type virtualMachines -o table\` to see what is available.`),
    );
  }

  for (const p of preferences) {
    if (usable.has(p.toLowerCase())) return p;
  }

  throw new Error(
    `None of the preferred ${poolLabel} VM sizes are available to your subscription in this region ` +
      `(${preferences.join(", ")}). Pass an explicit size with ${flagName}, or inspect ` +
      `\`az vm list-skus -l <region> --resource-type virtualMachines -o table\`.`,
  );
}

export interface ResolvedVmSizes {
  node: string;
  system: string;
  /** false when the SKU list could not be queried (fell back to defaults/overrides). */
  checked: boolean;
}

/**
 * Resolve usable node (sandbox/user) and system pool SKUs for a region by
 * querying `az vm list-skus`. If the query fails (no az, offline, permissions),
 * fall back to the caller's overrides or the historical defaults rather than
 * blocking the deploy — the bicep preflight will still surface a clear error.
 */
export async function resolveVmSizes(
  region: string,
  requestedNode?: string,
  requestedSystem?: string,
): Promise<ResolvedVmSizes> {
  let skus: VmSku[];
  try {
    const { stdout } = await execa(
      "az",
      [
        "vm",
        "list-skus",
        "--location",
        region,
        "--resource-type",
        "virtualMachines",
        "--output",
        "json",
      ],
      { stdio: "pipe" },
    );
    skus = JSON.parse(stdout) as VmSku[];
  } catch {
    return {
      node: requestedNode ?? USER_POOL_VM_PREFERENCES[0],
      system: requestedSystem ?? SYSTEM_POOL_VM_PREFERENCES[0],
      checked: false,
    };
  }

  const usable = usableSkuSet(skus);
  const system = pickUsableVmSize({
    usable,
    preferences: SYSTEM_POOL_VM_PREFERENCES,
    poolLabel: "system",
    flagName: "--system-vm-size",
    requested: requestedSystem,
  });
  const node = pickUsableVmSize({
    usable,
    preferences: USER_POOL_VM_PREFERENCES,
    poolLabel: "sandbox",
    flagName: "--node-vm-size",
    requested: requestedNode,
  });

  return { node, system, checked: true };
}
