// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { isAksNotFound } from "../../lib/deployment-target.js";
import {
  assertControllerMutationAllowed, PrivateRootUpgradeBlocked, type RootGuardExecute,
} from "../../lib/private-root-upgrade-guard.js";

function confirmedAksAbsence(error: unknown): boolean {
  if (!error || typeof error !== "object" || !("stderr" in error) || typeof error.stderr !== "string") return false;
  const reasons = [...error.stderr.matchAll(/^(?:ERROR:\s*)?\(([A-Za-z]+)\)(?:\s|:|$)/gm)];
  return reasons.length === 1 && isAksNotFound({ stderr: `(${reasons[0]![1]})` });
}

/** Move the existing AKS detection/connection ahead of provisioning effects.
 * An absent AKS is a fresh install; an unreadable one is not absence. */
export async function preflightUpRoot(
  execute: RootGuardExecute, resourceGroup: string, cluster: string, skipInfra: boolean,
): Promise<boolean> {
  if (!skipInfra) {
    let state: string;
    try {
      state = (await execute("az", [
        "aks", "show", "-g", resourceGroup, "-n", cluster, "--query", "provisioningState", "-o", "tsv",
      ], { stdio: "pipe" })).stdout.trim();
    } catch (error) {
      if (confirmedAksAbsence(error)) return false;
      throw new PrivateRootUpgradeBlocked("unavailable");
    }
    if (state !== "Succeeded") throw new PrivateRootUpgradeBlocked("unavailable");
  }
  try {
    await execute("az", [
      "aks", "get-credentials", "--name", cluster, "--resource-group", resourceGroup,
      "--overwrite-existing", "--output", "none",
    ], { stdio: "pipe" });
  } catch {
    throw new PrivateRootUpgradeBlocked("unavailable");
  }
  await assertControllerMutationAllowed(execute);
  return true;
}
