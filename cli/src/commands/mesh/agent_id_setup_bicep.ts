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
import { requireBundledAsset } from "../../lib/repo-assets.js";

export interface BicepSetupOptions {
  clusterName?: string;
  resourceGroup?: string;
  region: string;
  serviceTree?: string;
  /// Credential mode: "ManagedIdentityImds" (Pattern A) or
  /// "WorkloadIdentity" (Pattern B). Defaults to ManagedIdentityImds
  /// for backward compatibility. The CLI's auto-mode does NOT use
  /// the Bicep path for switching modes — auto detection lives in
  /// `agent_id_setup.ts` and only invokes Bicep in CA-blocked
  /// scenarios where the chosen mode is already known.
  credentialMode?: "ManagedIdentityImds" | "WorkloadIdentity";
  /// AKS OIDC issuer URL — required when credentialMode=WorkloadIdentity.
  aksOidcIssuerUrl?: string;
  dryRun?: boolean;
}

export interface BicepSetupResult {
  tenantId: string;
  blueprintClientId: string;
  blueprintObjectId: string;
  controllerMiClientId: string;
  controllerMiResourceId: string;
  controllerMiPrincipalId: string;
  credentialMode: "ManagedIdentityImds" | "WorkloadIdentity";
  aksOidcIssuerUrl?: string;
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
  credentialMode: BicepOutput<string>;
  controllerMiClientId: BicepOutput<string>;
  controllerMiResourceId: BicepOutput<string>;
  controllerMiPrincipalId: BicepOutput<string>;
  aksOidcIssuerUrl?: BicepOutput<string>;
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
  // Resolve via the shared repo-or-bundled resolver: in a repo checkout
  // this finds `deploy/bicep/agent-id-trust.bicep`; in an npm-installed
  // CLI it finds the copy bundled into `dist/deploy/bicep/` by
  // scripts/bundle-deploy-assets.mjs. The previous repo-relative walk
  // missed the bundled location entirely, so the Bicep fallback failed
  // with "template not found" for OOTB (no-checkout) users.
  try {
    return requireBundledAsset("deploy/bicep/agent-id-trust.bicep");
  } catch {
    // Last-resort fallback (e.g. exotic packaging) — let `az deployment`
    // surface a clear "template not found" rather than throwing here.
    const here = path.dirname(fileURLToPath(import.meta.url));
    return path.resolve(here, "../../deploy/bicep/agent-id-trust.bicep");
  }
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
      credentialMode: opts.credentialMode ?? "ManagedIdentityImds",
      aksOidcIssuerUrl: opts.aksOidcIssuerUrl,
    };
  }

  const credentialMode = opts.credentialMode ?? "ManagedIdentityImds";
  if (credentialMode === "WorkloadIdentity") {
    if (!opts.aksOidcIssuerUrl || !opts.aksOidcIssuerUrl.trim()) {
      throw new Error(
        "credentialMode=WorkloadIdentity requires aksOidcIssuerUrl. " +
          "Discover via: az aks show -n <cluster> -g <rg> --query oidcIssuerProfile.issuerUrl -o tsv",
      );
    }
  }
  kvLine("Credential mode", credentialMode);

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
    "--parameters",
    `credentialMode=${credentialMode}`,
  ];
  if (serviceTree) {
    args.push("--parameters", `serviceManagementReference=${serviceTree}`);
  }
  if (credentialMode === "WorkloadIdentity" && opts.aksOidcIssuerUrl) {
    args.push("--parameters", `aksOidcIssuerUrl=${opts.aksOidcIssuerUrl}`);
  }

  console.log();
  console.log(chalk.dim("  Running `az deployment sub create` — typical duration 30-90s..."));
  let deploymentResp: DeploymentResult;
  try {
    // Capture both streams: az emits linter warnings to stderr but
    // we don't want those misclassified as fatal. Also use `all: true`
    // so any actual deployment error from stdout makes it into the
    // error path. Timeout: 5 min for the Microsoft.Graph extension.
    const res = await execa("az", [...args, "-o", "json"], {
      stdio: ["ignore", "pipe", "pipe"],
      all: true,
      timeout: 5 * 60 * 1000,
    });
    if (!res.stdout || res.stdout.trim() === "") {
      throw new Error(
        `az returned no JSON output — stderr was: ${(res.stderr ?? "").slice(0, 400)}`,
      );
    }
    deploymentResp = JSON.parse(res.stdout) as DeploymentResult;
  } catch (e) {
    const err = e as {
      stderr?: string;
      stdout?: string;
      all?: string;
      message?: string;
    };
    // Prefer the merged stream `all` (captures the real error), then
    // stdout, then stderr. Strip the noisy Bicep linter `WARNING:`
    // prefix lines so the actual ARM error is what the user sees.
    const raw = err.all ?? err.stdout ?? err.stderr ?? err.message ?? "deployment failed";
    const cleaned = raw
      .split("\n")
      .filter((line) => !line.trimStart().startsWith("WARNING:"))
      .join("\n")
      .trim();
    // Prefer the line carrying the actual Graph/ARM rejection over the generic
    // first line ("Deployment failed. Correlation id: ...") so the surfaced
    // hint is the real cause, not boilerplate.
    const lines = cleaned.split("\n").map((l) => l.trim()).filter(Boolean);
    const meaningful =
      lines.find((l) =>
        /ServiceManagementReference|Authorization_RequestDenied|InvalidFederatedIdentity|insufficient privileges|forbidden|does not have permission/i.test(
          l,
        ),
      ) ?? lines[0] ?? raw.split("\n")[0] ?? "deployment failed";
    const summary = meaningful;

    // App registration in a Microsoft-corporate tenant needs a valid
    // ServiceTree GUID. Only steer the user to `--service-tree` when they did
    // NOT already pass one — otherwise the GUID they supplied is being
    // rejected (wrong value, or their account isn't linked to that service),
    // and telling them to "re-run with --service-tree" is misleading.
    if (/ServiceTree|ServiceManagementReference/i.test(raw)) {
      if (!serviceTree) {
        throw new Error(
          "Bicep deployment failed: your tenant requires a valid ServiceTree GUID for app " +
            "registration. Re-run with `--service-tree <GUID>` (or set KARS_SERVICE_TREE). " +
            `Underlying error: ${summary.slice(0, 240)}`,
        );
      }
      throw new Error(
        `Bicep deployment failed: the ServiceTree GUID you supplied (${serviceTree}) was rejected ` +
          "by the tenant. Confirm it's a valid ServiceTree *service* GUID and that your account is " +
          "associated with it (the value must match a real service, not just be a well-formed GUID). " +
          `Underlying error: ${summary.slice(0, 240)}`,
      );
    }

    throw new Error(`Bicep deployment failed: ${summary.slice(0, 400)}`);
  }

  const outputs = deploymentResp.properties.outputs;
  const result: BicepSetupResult = {
    tenantId: outputs.tenantId.value,
    blueprintClientId: outputs.blueprintClientId.value,
    blueprintObjectId: outputs.blueprintObjectId.value,
    controllerMiClientId: outputs.controllerMiClientId.value,
    controllerMiResourceId: outputs.controllerMiResourceId.value,
    controllerMiPrincipalId: outputs.controllerMiPrincipalId.value,
    credentialMode: (outputs.credentialMode?.value as
      | "ManagedIdentityImds"
      | "WorkloadIdentity") ?? credentialMode,
    aksOidcIssuerUrl: outputs.aksOidcIssuerUrl?.value || undefined,
  };

  // ── Write the KarsAuthConfig CR ──────────────────────────────────
  // Same shape as the imperative path. Wrapped in try/catch so a
  // missing CRD is reported clearly instead of bubbling up a raw
  // kubectl error.
  const controllerBlock: Record<string, unknown> = {
    credentialMode: result.credentialMode,
  };
  if (result.credentialMode === "ManagedIdentityImds") {
    if (result.controllerMiClientId)
      controllerBlock.managedIdentityClientId = result.controllerMiClientId;
    if (result.controllerMiResourceId)
      controllerBlock.managedIdentityResourceId = result.controllerMiResourceId;
    if (result.controllerMiPrincipalId)
      controllerBlock.managedIdentityPrincipalId = result.controllerMiPrincipalId;
  }
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
      controller: controllerBlock,
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
      stdio: ["pipe", "pipe", "pipe"],
    });
    console.log();
    console.log(chalk.green("  ✓ KarsAuthConfig/default written"));
    await printPortalVisibilityHint(result.blueprintObjectId);
  } catch (e) {
    // The kubectl apply is the LAST step and a soft failure here
    // should NOT mask the successful Bicep deployment that already
    // produced all the Entra resources. Check the execa error's
    // stderr (not just .message — execa.message is just "Command
    // failed…") to detect the CRD-missing case and surface a
    // helpful workaround instead of a generic "deployment failed".
    const err = e as { stderr?: string; stdout?: string; message?: string };
    const detail = (err.stderr ?? err.stdout ?? err.message ?? "").toString();
    if (detail.includes("no matches for kind")) {
      console.log();
      console.log(
        chalk.yellow(
          "  ⚠ Bicep deployment succeeded, but KarsAuthConfig CRD is not installed in the current kubectl context.",
        ),
      );
      console.log(
        chalk.dim(
          "    All Entra resources are already created — just install the CRD and re-run:",
        ),
      );
      console.log(chalk.cyan("      helm upgrade kars deploy/helm/kars -n kars-system --reuse-values"));
      console.log(chalk.cyan("      kars mesh setup-trust --mode bicep"));
      console.log();
      console.log(chalk.dim("    Bicep outputs (apply manually if you prefer):"));
      console.log(chalk.dim(`      blueprint:    ${result.blueprintClientId}`));
      console.log(chalk.dim(`      controller MI: ${result.controllerMiClientId}`));
      await printPortalVisibilityHint(result.blueprintObjectId);
      // Return the result so callers know the Bicep half succeeded.
      return result;
    }
    // Any other kubectl error IS a real problem — surface it but
    // still tagged as the kubectl step so the user knows the
    // Bicep half succeeded.
    throw new Error(`Bicep succeeded but kubectl apply KarsAuthConfig failed: ${detail.split("\n")[0].slice(0, 300)}`);
  }

  return result;
}

/// Print a ready-to-paste Graph Explorer PATCH body that upgrades the
/// untyped blueprint App to the typed `agentIdentityBlueprint` form.
///
/// Why this exists: the Bicep `Microsoft.Graph` extension does not
/// (as of writing) support the `@odata.type` discriminator needed to
/// create a typed `agentIdentityBlueprint`. Bicep produces a regular
/// `Application` tagged `EntraAgentId` — fully functional for the
/// runtime, but **not visible** under
/// Entra portal → Identity → Agents → Agent identity blueprints.
///
/// Empirically the portal filter requires three things on top of the
/// `@odata.type` upgrade: `sponsors`, `owners`, and (when present)
/// the `serviceManagementReference`. Including `sponsors`+`owners`
/// in the PATCH is what makes the blueprint show up in the Agents
/// page — a minimal `@odata.type`-only PATCH does the type upgrade
/// but the entry stays hidden.
///
/// We can't run the PATCH on the user's behalf because the CA policy
/// (AADSTS530084) that pushed us to Bicep in the first place also
/// blocks `az rest` to Graph. The next-best thing is to print a
/// fully-populated body the user can paste into Graph Explorer, which
/// uses a different first-party app (`de8bc8b5-...`) and bypasses
/// the CA token-binding policy.
///
/// User OID discovery: try `az ad signed-in-user show` first (works
/// in tenants where ARM-scope Graph is allowed but data-plane Graph
/// is blocked — rare but exists). On failure leave a clear
/// `<YOUR_USER_OID>` sentinel with a one-line hint on how to find it.
async function printPortalVisibilityHint(blueprintObjectId: string): Promise<void> {
  const userOid = await tryGetSignedInUserOid();
  const oid = userOid ?? "<YOUR_USER_OID>";
  const body = JSON.stringify(
    {
      "@odata.type": "Microsoft.Graph.AgentIdentityBlueprint",
      "sponsors@odata.bind": [`https://graph.microsoft.com/v1.0/users/${oid}`],
      "owners@odata.bind": [`https://graph.microsoft.com/v1.0/users/${oid}`],
    },
    null,
    2,
  );
  console.log();
  console.log(
    chalk.dim(
      "  ── Make blueprint visible under Entra → Agents (optional) ──",
    ),
  );
  console.log(
    chalk.dim(
      "  Bicep's Microsoft.Graph extension cannot set @odata.type, so the",
    ),
  );
  console.log(
    chalk.dim(
      "  blueprint works but is not listed in the Agents portal page.",
    ),
  );
  console.log(
    chalk.dim(
      "  To make it visible, paste this PATCH into Graph Explorer:",
    ),
  );
  console.log(chalk.cyan("    https://developer.microsoft.com/en-us/graph/graph-explorer"));
  console.log();
  console.log(
    chalk.cyan(
      `    PATCH https://graph.microsoft.com/beta/applications/${blueprintObjectId}`,
    ),
  );
  console.log(chalk.cyan("    Content-Type: application/json"));
  console.log();
  for (const line of body.split("\n")) {
    console.log(chalk.cyan(`    ${line}`));
  }
  if (!userOid) {
    console.log();
    console.log(
      chalk.dim(
        "  Replace <YOUR_USER_OID> with your Entra user objectId — find it at",
      ),
    );
    console.log(
      chalk.dim(
        "  Entra admin center → Users → (your account) → Object ID.",
      ),
    );
  }
  console.log();
  console.log(
    chalk.dim(
      "  ── If the PATCH is rejected (delete + recreate path) ──",
    ),
  );
  console.log(
    chalk.dim(
      "  Some tenants reject in-place @odata.type upgrades. In that case",
    ),
  );
  console.log(
    chalk.dim(
      "  delete the existing app and recreate it typed — but remember a",
    ),
  );
  console.log(
    chalk.dim(
      "  fresh Graph Explorer POST /applications creates ONLY the app:",
    ),
  );
  console.log(
    chalk.dim(
      "  the SP and FIC have to be recreated too. Bicep did all three;",
    ),
  );
  console.log(
    chalk.dim(
      "  the Graph Explorer fallback only does step 1. Full sequence:",
    ),
  );
  console.log();
  console.log(
    chalk.cyan(
      "    # 1. Create typed app (note the new objectId — call it $NEW_OID)",
    ),
  );
  console.log(
    chalk.cyan(
      "    POST https://graph.microsoft.com/beta/applications",
    ),
  );
  console.log(
    chalk.cyan(
      "    Body: { \"@odata.type\": \"Microsoft.Graph.AgentIdentityBlueprint\",",
    ),
  );
  console.log(
    chalk.cyan(
      "            \"displayName\": \"kars-blueprint\",",
    ),
  );
  console.log(
    chalk.cyan(
      `            "sponsors@odata.bind": ["https://graph.microsoft.com/v1.0/users/${oid}"],`,
    ),
  );
  console.log(
    chalk.cyan(
      `            "owners@odata.bind":   ["https://graph.microsoft.com/v1.0/users/${oid}"] }`,
    ),
  );
  console.log();
  console.log(
    chalk.cyan(
      "    # 2. Create SP for the new app",
    ),
  );
  console.log(
    chalk.cyan(
      "    POST https://graph.microsoft.com/v1.0/servicePrincipals",
    ),
  );
  console.log(
    chalk.cyan(
      "    Body: { \"appId\": \"<NEW_APP_ID_FROM_STEP_1>\" }",
    ),
  );
  console.log();
  console.log(
    chalk.cyan(
      "    # 3. Recreate the MI-as-FIC on the new app",
    ),
  );
  console.log(
    chalk.cyan(
      "    POST https://graph.microsoft.com/v1.0/applications/$NEW_OID/federatedIdentityCredentials",
    ),
  );
  console.log(
    chalk.cyan(
      "    Body: { \"name\": \"kars-controller-mi-fic\",",
    ),
  );
  console.log(
    chalk.cyan(
      "            \"issuer\": \"https://login.microsoftonline.com/<TENANT>/v2.0\",",
    ),
  );
  console.log(
    chalk.cyan(
      "            \"subject\": \"<MI_PRINCIPAL_FROM_KarsAuthConfig>\",",
    ),
  );
  console.log(
    chalk.cyan(
      "            \"audiences\": [\"api://AzureADTokenExchange\"] }",
    ),
  );
  console.log();
  console.log(
    chalk.cyan(
      "    # 4. Re-point the cluster CR + delete the old orphan app",
    ),
  );
  console.log(
    chalk.cyan(
      "    kubectl patch karsauthconfig default --type=merge \\",
    ),
  );
  console.log(
    chalk.cyan(
      "      -p '{\"spec\":{\"agentId\":{\"blueprintClientId\":\"<NEW>\",\"blueprintObjectId\":\"<NEW>\"}}}'",
    ),
  );
  console.log(
    chalk.cyan(
      `    DELETE https://graph.microsoft.com/v1.0/applications/${blueprintObjectId}`,
    ),
  );
  console.log();
}

/// Best-effort: try to read the signed-in user's Entra objectId via
/// `az ad signed-in-user show`. This usually hits the same CA block
/// as direct Graph calls (returns AADSTS530084) — we return null
/// silently in that case so the caller falls back to the literal sentinel.
async function tryGetSignedInUserOid(): Promise<string | null> {
  try {
    const res = await execa(
      "az",
      ["ad", "signed-in-user", "show", "--query", "id", "-o", "tsv"],
      { stdio: ["ignore", "pipe", "pipe"], timeout: 15_000 },
    );
    const oid = res.stdout.trim();
    // Quick GUID sanity check — az returns the error string on stdout
    // in some configurations rather than failing the process.
    if (/^[0-9a-f-]{36}$/i.test(oid)) {
      return oid;
    }
    return null;
  } catch {
    return null;
  }
}
