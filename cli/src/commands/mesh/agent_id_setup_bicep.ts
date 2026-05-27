// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Entra Agent ID trust provisioning via Bicep ARM deployment.
//!
//! Alternative to `agent_id_setup.ts` for tenants where the Azure CLI
//! cannot acquire a Microsoft Graph token because of Conditional
//! Access token-binding policy (AADSTS530084). Bicep goes through
//! ARM's deployment engine, which uses the `Microsoft.Graph` Bicep
//! extension on the resource-provider side. ARM has its own auth
//! path to Graph and is not subject to the same CA policy.
//!
//! Same end state as the CLI path: blueprint app + SP + controller
//! managed identity + MI-as-FIC on the blueprint + KarsAuthConfig CR
//! in the cluster. Same idempotence guarantees — re-running on a
//! tenant that already has the blueprint is a no-op.

import chalk from "chalk";
import { execa } from "execa";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { kvLine, section } from "../../stepper.js";

export interface BicepSetupOptions {
  clusterName?: string;
  resourceGroup?: string;
  region: string;
  serviceTree?: string;
  dryRun?: boolean;
}

export interface BicepSetupResult {
  tenantId: string;
  blueprintClientId: string;
  blueprintObjectId: string;
  controllerMiClientId: string;
  controllerMiResourceId: string;
  controllerMiPrincipalId: string;
}

interface AzAccount {
  id: string;
  tenantId: string;
  user: { name: string };
}

interface BicepOutput<T> {
  type: string;
  value: T;
}

interface BicepOutputs {
  tenantId: BicepOutput<string>;
  blueprintClientId: BicepOutput<string>;
  blueprintObjectId: BicepOutput<string>;
  blueprintSpObjectId: BicepOutput<string>;
  controllerMiClientId: BicepOutput<string>;
  controllerMiResourceId: BicepOutput<string>;
  controllerMiPrincipalId: BicepOutput<string>;
}

interface DeploymentResult {
  properties: {
    outputs: BicepOutputs;
    provisioningState: string;
  };
}

/// Locate the bundled Bicep template. Resolves relative to the CLI
/// package layout so the path works whether kars is run via
/// `npm link` from source, the published @kars/cli npm package, or
/// the prebuilt binary.
///
/// Walks up from the source file's directory looking for the
/// `deploy/bicep/agent-id-trust.bicep` anchor. Falls back to the
/// repo-relative path during local dev.
function resolveBicepTemplate(): string {
  const here = path.dirname(fileURLToPath(import.meta.url));
  // From cli/src/commands/mesh/ → ../../../../deploy/bicep/...
  // Also handle dist/commands/mesh/ → ../../../../deploy/bicep/...
  const candidates = [
    path.resolve(here, "../../../../deploy/bicep/agent-id-trust.bicep"),
    path.resolve(here, "../../../deploy/bicep/agent-id-trust.bicep"),
    path.resolve(process.cwd(), "deploy/bicep/agent-id-trust.bicep"),
  ];
  for (const c of candidates) {
    // We cannot import 'fs' lazily inside the helper without changing
    // the surrounding test mock surface; use a small sync existsSync
    // ESM import here to keep mocked `execa` calls clean.
    try {
      // eslint-disable-next-line @typescript-eslint/no-require-imports
      const { existsSync } = require("fs");
      if (existsSync(c)) return c;
    } catch {
      /* env without sync require — fall through */
    }
  }
  // Best-effort default — `az deployment` will error clearly if missing.
  return candidates[0];
}

async function getTenantInfo(): Promise<{ tenantId: string; subscriptionId: string; user: string }> {
  const res = await execa("az", ["account", "show", "-o", "json"], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  const account = JSON.parse(res.stdout) as AzAccount;
  if (!account?.tenantId) {
    throw new Error("Azure CLI is not signed in — run `az login` first.");
  }
  return {
    tenantId: account.tenantId,
    subscriptionId: account.id,
    user: account.user?.name ?? "<unknown>",
  };
}

/// Run the Bicep deployment + materialise the KarsAuthConfig CR.
///
/// Idempotent at the Bicep layer (Graph resources with `uniqueName`
/// are upserted by re-deploying) and at the K8s layer (`kubectl
/// apply` overwrites the singleton CR).
export async function ensureAgentIdTrustViaBicep(
  opts: BicepSetupOptions,
): Promise<BicepSetupResult> {
  const auth = await getTenantInfo();
  const clusterName = opts.clusterName ?? "kars";
  const rg = opts.resourceGroup ?? `${clusterName}-agentid-rg`;
  const region = opts.region;
  const serviceTree =
    opts.serviceTree && opts.serviceTree.trim()
      ? opts.serviceTree.trim()
      : (process.env.KARS_SERVICE_TREE ?? "").trim() || undefined;

  const bicepPath = resolveBicepTemplate();
  const deploymentName = `kars-agentid-${Date.now()}`;

  section("Entra Agent ID — Bicep deployment");
  kvLine("Tenant", auth.tenantId);
  kvLine("Subscription", auth.subscriptionId);
  kvLine("Signed in as", auth.user);
  kvLine("Cluster", clusterName);
  kvLine("Region", region);
  kvLine("Resource group", rg);
  if (serviceTree) kvLine("Service tree GUID", serviceTree);
  kvLine("Bicep template", bicepPath);
  kvLine("Deployment name", deploymentName);

  if (opts.dryRun) {
    console.log(chalk.yellow("\n  ⚠ --dry-run: no changes were made."));
    return {
      tenantId: auth.tenantId,
      blueprintClientId: "<dry-run>",
      blueprintObjectId: "<dry-run>",
      controllerMiClientId: "<dry-run>",
      controllerMiResourceId: "<dry-run>",
      controllerMiPrincipalId: "<dry-run>",
    };
  }

  // ── Run the deployment at subscription scope ─────────────────────
  const args = [
    "deployment",
    "sub",
    "create",
    "--name",
    deploymentName,
    "--location",
    region,
    "--template-file",
    bicepPath,
    "--parameters",
    `clusterName=${clusterName}`,
    "--parameters",
    `resourceGroupName=${rg}`,
    "--parameters",
    `region=${region}`,
  ];
  if (serviceTree) {
    args.push("--parameters", `serviceManagementReference=${serviceTree}`);
  }

  console.log();
  console.log(chalk.dim("  Running `az deployment sub create` — typical duration 30-90s..."));
  let deploymentResp: DeploymentResult;
  try {
    const res = await execa("az", [...args, "-o", "json"], {
      stdio: ["ignore", "pipe", "pipe"],
      // Bicep + Graph extension provisioning can take a while; give
      // it 5 minutes before failing.
      timeout: 5 * 60 * 1000,
    });
    deploymentResp = JSON.parse(res.stdout) as DeploymentResult;
  } catch (e) {
    const err = e as { stderr?: string; message?: string };
    const detail = err.stderr?.trim() ?? err.message ?? "deployment failed";
    throw new Error(`Bicep deployment failed: ${detail.split("\n")[0]}`);
  }

  const outputs = deploymentResp.properties.outputs;
  const result: BicepSetupResult = {
    tenantId: outputs.tenantId.value,
    blueprintClientId: outputs.blueprintClientId.value,
    blueprintObjectId: outputs.blueprintObjectId.value,
    controllerMiClientId: outputs.controllerMiClientId.value,
    controllerMiResourceId: outputs.controllerMiResourceId.value,
    controllerMiPrincipalId: outputs.controllerMiPrincipalId.value,
  };

  // ── Write the KarsAuthConfig CR ──────────────────────────────────
  // Same shape as the imperative path. Wrapped in try/catch so a
  // missing CRD is reported clearly instead of bubbling up a raw
  // kubectl error.
  const cr = {
    apiVersion: "kars.azure.com/v1alpha1",
    kind: "KarsAuthConfig",
    metadata: { name: "default" },
    spec: {
      tenant: {
        tenantId: result.tenantId,
        authorityHost: "https://login.microsoftonline.com/",
        ...(serviceTree ? { serviceManagementReference: serviceTree } : {}),
      },
      agentId: {
        blueprintClientId: result.blueprintClientId,
        blueprintObjectId: result.blueprintObjectId,
      },
      controller: {
        managedIdentityClientId: result.controllerMiClientId,
        managedIdentityResourceId: result.controllerMiResourceId,
        managedIdentityPrincipalId: result.controllerMiPrincipalId,
      },
      downstreamApis: {
        Foundry: {
          baseUrl: "https://ai.azure.com/",
          scopes: ["https://ai.azure.com/.default"],
          requestAppToken: true,
        },
        Graph: {
          baseUrl: "https://graph.microsoft.com/v1.0/",
          scopes: ["https://graph.microsoft.com/.default"],
          requestAppToken: true,
        },
      },
    },
  };

  try {
    await execa("kubectl", ["apply", "-f", "-"], {
      input: JSON.stringify(cr),
      stdio: ["pipe", "inherit", "inherit"],
    });
  } catch (e) {
    const msg = (e as Error).message;
    if (msg.includes("no matches for kind")) {
      console.log(
        chalk.yellow(
          "\n  ⚠ KarsAuthConfig CRD not installed in the current kubectl context.",
        ),
      );
      console.log(
        chalk.dim(
          "    Run `helm upgrade kars deploy/helm/kars -n kars-system --reuse-values` and retry.",
        ),
      );
    } else {
      throw e;
    }
  }

  return result;
}
