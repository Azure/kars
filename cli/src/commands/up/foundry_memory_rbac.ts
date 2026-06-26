// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// up/foundry_memory_rbac.ts — make the Foundry **Memory Store** actually
// persist, idempotently.
//
// The Memory Store is the one Foundry capability whose `update`/`search`
// operations call models INTERNALLY, authenticating as the Foundry
// PROJECT's managed identity (not the caller). So the store CRUD can
// succeed for the caller while every persist/recall 403s — the classic
// "store creates but nothing sticks" symptom — unless the project MI:
//   1. exists (system-assigned MI enabled on the project), and
//   2. holds `Azure AI User` on the resource group hosting the account, and
//   3. the project has an embedding model deployed.
//
// `setupFoundryForKars` already (1) enables the MI and (3) deploys an
// embedding model. This module adds (2) — the resource-group role grant —
// and bundles all three into a single idempotent call that both `kars up`
// and `kars upgrade` run, so the fix rides along with any upgrade instead
// of requiring a fresh `kars up`.

import type { Stepper } from "../../stepper.js";
import { parseFoundryEndpoint, setupFoundryForKars } from "./foundry_setup.js";

type Execa = typeof import("execa").execa;

/** Built-in role definition GUID for **Azure AI User** (a.k.a. "Foundry
 *  User") — carries the `Microsoft.CognitiveServices/*` data actions the
 *  Memory Store needs for its internal chat + embedding calls. */
export const AZURE_AI_USER_ROLE_ID = "53ca6127-db72-4b80-b1b0-d745d6d5456d";

export interface FoundryMemoryRbacResult {
  /** True when the project MI holds the role afterwards (granted now or
   *  already present). */
  granted: boolean;
  /** Project system-assigned MI principalId, or "" when unavailable. */
  projectMiPrincipalId: string;
  /** Embedding deployment name in use, or undefined. */
  embeddingModel?: string;
  /** Human-readable status notes for the report. */
  notes: string[];
}

/** Build the `az role assignment create` argv granting `principalId` the
 *  Azure AI User role at resource-group scope. `az` treats a duplicate
 *  create as a success (no-op), so this is safe to re-run. Exported for
 *  tests. */
export function buildProjectMiRoleAssignmentArgs(
  principalId: string,
  resourceGroupId: string,
): string[] {
  return [
    "role", "assignment", "create",
    "--assignee-object-id", principalId,
    "--assignee-principal-type", "ServicePrincipal",
    "--role", AZURE_AI_USER_ROLE_ID,
    "--scope", resourceGroupId,
    "--output", "none",
  ];
}

/** A 403/RoleAssignmentExists message from `az` means the assignment is
 *  already there — that's success for our idempotent intent. */
function isAlreadyExists(message: string): boolean {
  return /RoleAssignmentExists|already exists/i.test(message);
}

/**
 * Idempotently ensure the bound Foundry project can run Memory Store
 * persistence: enable the project MI, ensure an embedding deployment, and
 * grant the project MI `Azure AI User` on the resource group. Best-effort
 * and non-fatal — every failure degrades to an actionable note. Returns a
 * structured result so callers can surface a clear status.
 *
 * `foundryEndpoint` must be a Foundry PROJECT endpoint
 * (`https://<acct>.services.ai.azure.com/api/projects/<proj>`); a plain
 * Azure OpenAI endpoint has no Memory Store and is skipped.
 */
export async function ensureFoundryMemoryRbac(args: {
  execa: Execa;
  stepper: Stepper;
  foundryEndpoint: string;
}): Promise<FoundryMemoryRbacResult> {
  const { execa, stepper, foundryEndpoint } = args;
  const notes: string[] = [];

  if (!parseFoundryEndpoint(foundryEndpoint)) {
    return {
      granted: false,
      projectMiPrincipalId: "",
      notes: ["No Foundry project endpoint bound — Memory Store RBAC not applicable."],
    };
  }

  const setup = await setupFoundryForKars({ execa, stepper, foundryEndpoint }).catch(() => null);
  if (!setup) {
    return {
      granted: false,
      projectMiPrincipalId: "",
      notes: ["Foundry project auto-setup did not run — Memory Store RBAC skipped."],
    };
  }
  notes.push(...setup.notes);

  const principalId = setup.projectMiPrincipalId;
  if (!principalId) {
    notes.push(
      "Foundry project MI principalId is not available yet — Memory Store RBAC " +
        "not granted. Re-run after the identity propagates (a minute or two).",
    );
    return { granted: false, projectMiPrincipalId: "", embeddingModel: setup.embeddingModel, notes };
  }
  if (!setup.resourceGroup) {
    notes.push("Could not resolve the Foundry resource group — Memory Store RBAC not granted.");
    return { granted: false, projectMiPrincipalId: principalId, embeddingModel: setup.embeddingModel, notes };
  }

  // Resolve the resource-group ARM id (the role scope).
  const { stdout: rgIdRaw } = await execa("az", [
    "group", "show", "--name", setup.resourceGroup, "--query", "id", "--output", "tsv",
  ], { stdio: "pipe" }).catch(() => ({ stdout: "" }));
  const resourceGroupId = (rgIdRaw || "").trim().split("\n").pop()?.trim() || "";
  if (!resourceGroupId) {
    notes.push(
      `Could not resolve the ARM id for resource group '${setup.resourceGroup}' — ` +
        "Memory Store RBAC not granted.",
    );
    return { granted: false, projectMiPrincipalId: principalId, embeddingModel: setup.embeddingModel, notes };
  }

  stepper.update("Granting Foundry project MI 'Azure AI User' on the resource group...");
  const granted = await execa(
    "az",
    buildProjectMiRoleAssignmentArgs(principalId, resourceGroupId),
    { stdio: "pipe" },
  )
    .then(() => true)
    .catch((e: unknown) => isAlreadyExists(e instanceof Error ? e.message : String(e)));

  if (granted) {
    notes.push(
      "Foundry project MI holds 'Azure AI User' on the resource group " +
        "(Memory Store internal model calls). RBAC can take a few minutes to propagate.",
    );
  } else {
    notes.push(
      "Could not grant the Foundry project MI 'Azure AI User' on the resource group " +
        "(needs Owner or User Access Administrator). Grant it once, then Memory Store " +
        "persistence will work: az role assignment create --assignee-object-id " +
        `${principalId} --role "${AZURE_AI_USER_ROLE_ID}" --scope ${resourceGroupId}`,
    );
  }

  return { granted, projectMiPrincipalId: principalId, embeddingModel: setup.embeddingModel, notes };
}
