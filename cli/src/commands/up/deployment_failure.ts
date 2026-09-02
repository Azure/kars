// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import chalk from "chalk";
import type { Stepper } from "../../stepper.js";
import {
  cleanupCreatedResourceGroup,
  formatCleanupCompletion,
  formatRetainedResourceGuidance,
  maybeRollbackResourceGroup,
  ResourceGroupOwnershipError,
  type AzureRunner,
  type CleanupContext,
  type CleanupResult,
  type ResourceGroupOwnershipProof,
} from "./orchestration.js";

export interface DeploymentFailureInput {
  error: unknown;
  stepper: Stepper;
  resourceGroup: string;
  cleanupContext?: CleanupContext;
  resourceGroupOwnership?: ResourceGroupOwnershipProof;
  runAzure: AzureRunner;
  cleanupAndClearDeploymentContext: (
    cleanup: () => Promise<CleanupResult>,
  ) => Promise<CleanupResult>;
}

export async function reportDeploymentFailure({
  error,
  stepper,
  resourceGroup,
  cleanupContext,
  resourceGroupOwnership,
  runAzure,
  cleanupAndClearDeploymentContext,
}: DeploymentFailureInput): Promise<void> {
  stepper.stop();
  console.error(chalk.red(`\n  Deployment failed`));
  const message = error instanceof Error ? error.message : String(error);
  console.error(chalk.red(`  ${message}\n`));

  if (message.includes("EncryptionAtHost")) {
    console.log(
      chalk.yellow(
        "  Tip: EncryptionAtHost requires registering the feature:",
      ),
    );
    console.log(
      chalk.cyan(
        "  az feature register --namespace Microsoft.Compute --name EncryptionAtHost",
      ),
    );
    console.log(
      chalk.cyan("  az provider register -n Microsoft.Compute\n"),
    );
  }
  if (!cleanupContext) {
    return;
  }

  let cleanupResult: CleanupResult | undefined;
  try {
    const disposition = await maybeRollbackResourceGroup({
      ownershipProof: resourceGroupOwnership,
      cleanup: async () => {
        console.log(
          chalk.yellow(
            `  Cleaning up resource group '${resourceGroup}' created by this run...`,
          ),
        );
        cleanupResult = await cleanupAndClearDeploymentContext(() =>
          cleanupCreatedResourceGroup(cleanupContext, runAzure),
        );
      },
    });

    if (disposition === "cleaned") {
      for (const cleanupLine of formatCleanupCompletion(
        resourceGroup,
        cleanupResult ?? {
          keyVaultNames: [],
          azureAiNames: [],
          purgeFailures: [],
        },
      )) {
        console.log(
          (cleanupResult?.purgeFailures.length ? chalk.yellow : chalk.green)(
            `  ${cleanupLine}`,
          ),
        );
      }
    } else {
      console.log(
        chalk.dim(
          `  Resource group '${resourceGroup}' was preserved because it existed before this invocation. Fix the error and retry the same command.\n`,
        ),
      );
    }
  } catch (cleanupError) {
    const cleanupMessage =
      cleanupError instanceof Error
        ? cleanupError.message
        : String(cleanupError);
    console.error(
      chalk.red(`  Automatic cleanup failed: ${cleanupMessage}\n`),
    );
    if (cleanupError instanceof ResourceGroupOwnershipError) {
      console.log(
        chalk.yellow(
          `  Resource group '${resourceGroup}' was not deleted because the rollback safety protocol could not prove exclusive deletion was safe. It may have been adopted concurrently; inspect its locks and resources manually.\n`,
        ),
      );
    } else if (resourceGroupOwnership) {
      console.log(
        chalk.yellow(`${formatRetainedResourceGuidance(cleanupContext)}\n`),
      );
    }
  }
}
