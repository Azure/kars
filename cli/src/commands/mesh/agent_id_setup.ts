// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Auto-provisioning of the Entra Agent ID trust anchor.
//!
//! Used by both `kars up` (transparent, called automatically when the
//! cluster is missing tenant trust) and `kars mesh setup-trust`
//! (explicit user invocation for power users or CI). The CLI surface
//! is identical — the module exposes `ensureAgentIdTrust(opts)` which
//! is idempotent: if every step already succeeded for this cluster,
//! it short-circuits and returns the existing IDs without touching
//! Microsoft Graph.
//!
//! ## Phases of provisioning
//!
//! 1. Az auth check — must be `az login`-ed with `Agent ID Developer`
//!    role available.
//! 2. Blueprint app: created via `az rest` against Microsoft Graph
//!    (NOT `az ad app create` — that wires up a regular Application,
//!    not the derived `agentIdentityBlueprint` type we need).
//! 3. Blueprint service principal: required for the blueprint to be
//!    visible in the Entra portal and for agent identities to derive
//!    from it. Created via `az ad sp create --id <blueprint-app-id>`.
//! 4. Controller managed identity: an ARM `userAssignedIdentities`
//!    resource in the customer's subscription. The MI's principalId
//!    becomes the subject of the blueprint's federated identity
//!    credential. The MI is later assigned to the AKS sandbox node
//!    pool VMSS (in `up.ts`, where the AKS cluster RG is known).
//! 5. Federated identity credential on the blueprint: the trust hop
//!    that lets the controller MI's IMDS token authenticate as the
//!    blueprint via `client_assertion_type=jwt-bearer`.
//! 6. KarsAuthConfig CR: written to the cluster via `kubectl apply`.
//!    The controller's `auth_config_reconciler` picks it up and
//!    materialises the sidecar env ConfigMap.
//!
//! Each phase is idempotent — running this function twice on the same
//! tenant + sub yields the same IDs and makes no API calls past the
//! existence-check.

import chalk from "chalk";
import { execa } from "execa";
import { kvLine, section } from "../../stepper.js";

/// Options accepted by `ensureAgentIdTrust`. All fields are optional;
/// sensible defaults are derived from the current `az account show`
/// when omitted.
export interface AgentIdSetupOptions {
  /// Cluster name (used to suffix the blueprint and controller MI).
  /// Defaults to "kars" — most users have one cluster per tenant.
  clusterName?: string;
  /// Subscription ID. Defaults to the currently-selected subscription
  /// from `az account show`.
  subscriptionId?: string;
  /// Resource group for the controller managed identity. Created if
  /// it does not exist. Defaults to "<clusterName>-agentid-rg".
  resourceGroup?: string;
  /// Azure region for the controller managed identity. Defaults to
  /// "eastus".
  region?: string;
  /// ServiceTree / service-management-reference GUID. Required in
  /// Microsoft corporate (and a few similarly-policed enterprise)
  /// tenants. Falls back to `KARS_SERVICE_TREE` env var if not
  /// passed explicitly.
  serviceTree?: string;
  /// If `true`, prints what would happen without making any changes.
  dryRun?: boolean;
}

/// Result of a successful auto-provision. The same shape is returned
/// whether the trust was created fresh or already existed.
export interface AgentIdSetupResult {
  tenantId: string;
  blueprintClientId: string;
  blueprintObjectId: string;
  controllerMiClientId: string;
  controllerMiResourceId: string;
  controllerMiPrincipalId: string;
  /// `true` when this invocation created the blueprint (vs.
  /// short-circuiting on an existing one). Useful for telling the
  /// user "first-time setup complete" vs "already wired up".
  freshlyCreated: boolean;
}

interface AzAccount {
  id: string;
  tenantId: string;
  user: { name: string };
}

async function azJson<T>(args: string[]): Promise<T | null> {
  try {
    const res = await execa("az", [...args, "-o", "json"], {
      stdio: ["ignore", "pipe", "pipe"],
    });
    if (!res.stdout || res.stdout.trim() === "" || res.stdout.trim() === "null") return null;
    return JSON.parse(res.stdout) as T;
  } catch (e: unknown) {
    const err = e as { stderr?: string; message?: string; exitCode?: number };
    const stderr = err.stderr ?? err.message ?? "";
    throw new Error(stderr.trim() || `az ${args.join(" ")} failed`);
  }
}

async function azGraphRest<T>(
  method: "GET" | "POST" | "PATCH" | "DELETE",
  graphPath: string,
  body?: unknown,
): Promise<T | null> {
  // Microsoft Graph requires the OData-Version header for derived
  // types like agentIdentityBlueprint. `az rest` does not let us set
  // headers directly via flag, but it forwards Authorization and
  // Content-Type by default; for OData-Version we use the
  // --headers flag (supported in az 2.44+).
  const args = [
    "rest",
    "--method",
    method,
    "--url",
    `https://graph.microsoft.com${graphPath}`,
    "--headers",
    "OData-Version=4.0",
  ];

  if (body !== undefined && body !== null) {
    args.push("--body", JSON.stringify(body));
  }

  try {
    const res = await execa("az", [...args], {
      stdio: ["ignore", "pipe", "pipe"],
    });
    if (!res.stdout || res.stdout.trim() === "") return null;
    return JSON.parse(res.stdout) as T;
  } catch (e: unknown) {
    const err = e as { stderr?: string; message?: string };
    throw new Error(err.stderr?.trim() ?? err.message ?? `az rest ${graphPath} failed`);
  }
}

async function ensureAzAuth(): Promise<{
  tenantId: string;
  subscriptionId: string;
  user: string;
}> {
  let account: AzAccount | null;
  try {
    account = await azJson<AzAccount>(["account", "show"]);
  } catch {
    throw new Error("Azure CLI is not signed in — run `az login` first.");
  }
  if (!account || !account.tenantId) {
    throw new Error("Azure CLI is not signed in — run `az login` first.");
  }
  return {
    tenantId: account.tenantId,
    subscriptionId: account.id,
    user: account.user?.name ?? "<unknown>",
  };
}

interface MeApiResponse {
  id: string;
  displayName: string;
  userPrincipalName: string;
}

async function getCurrentUserOid(): Promise<string> {
  // /me requires User.Read; Agent ID Developer implies this.
  const me = await azGraphRest<MeApiResponse>("GET", "/v1.0/me");
  if (!me || !me.id) {
    throw new Error("Failed to look up current user via Graph /me — Agent ID Developer role required");
  }
  return me.id;
}

interface BlueprintGraphResponse {
  id: string;
  appId: string;
  displayName: string;
  serviceManagementReference?: string | null;
}

/// Look up an existing blueprint by display name. Returns null when
/// none match. Display names are not unique in Graph, but we are
/// explicit about our naming convention so duplicates would be
/// operator error.
async function findExistingBlueprint(
  displayName: string,
): Promise<BlueprintGraphResponse | null> {
  const filter = encodeURIComponent(`displayName eq '${displayName}'`);
  interface ListResp {
    value: BlueprintGraphResponse[];
  }
  const resp = await azGraphRest<ListResp>(
    "GET",
    `/v1.0/applications?$filter=${filter}&$top=2`,
  );
  if (!resp || !resp.value || resp.value.length === 0) return null;
  if (resp.value.length > 1) {
    throw new Error(
      `Found ${resp.value.length} applications named '${displayName}' — refusing to disambiguate. Delete the unwanted ones manually.`,
    );
  }
  return resp.value[0];
}

async function createBlueprint(
  displayName: string,
  userOid: string,
  serviceTree: string | undefined,
): Promise<BlueprintGraphResponse> {
  const body: Record<string, unknown> = {
    "@odata.type": "#Microsoft.Graph.AgentIdentityBlueprint",
    displayName,
    "sponsors@odata.bind": [`https://graph.microsoft.com/v1.0/users/${userOid}`],
    "owners@odata.bind": [`https://graph.microsoft.com/v1.0/users/${userOid}`],
  };
  if (serviceTree && serviceTree.trim()) {
    body.serviceManagementReference = serviceTree.trim();
  }

  const created = await azGraphRest<BlueprintGraphResponse>(
    "POST",
    "/v1.0/applications/",
    body,
  );
  if (!created || !created.appId) {
    throw new Error("Graph POST /applications returned an empty response");
  }
  return created;
}

interface SpGraphResponse {
  id: string;
  appId: string;
  displayName: string;
}

async function ensureBlueprintSp(appId: string): Promise<SpGraphResponse> {
  // Look up first.
  interface ListResp {
    value: SpGraphResponse[];
  }
  const filter = encodeURIComponent(`appId eq '${appId}'`);
  const existing = await azGraphRest<ListResp>(
    "GET",
    `/v1.0/servicePrincipals?$filter=${filter}&$top=1`,
  );
  if (existing && existing.value && existing.value.length > 0) {
    return existing.value[0];
  }
  // Create.
  const created = await azGraphRest<SpGraphResponse>(
    "POST",
    "/v1.0/servicePrincipals",
    { appId },
  );
  if (!created || !created.id) {
    throw new Error("Graph POST /servicePrincipals returned an empty response");
  }
  return created;
}

interface ManagedIdentityResponse {
  id: string;
  clientId: string;
  principalId: string;
  name: string;
  location: string;
}

async function ensureResourceGroup(rg: string, region: string): Promise<void> {
  try {
    await azJson(["group", "show", "--name", rg]);
    return;
  } catch {
    // Doesn't exist yet — create.
  }
  await azJson(["group", "create", "--name", rg, "--location", region]);
}

async function ensureControllerMi(
  rg: string,
  region: string,
  miName: string,
): Promise<ManagedIdentityResponse> {
  try {
    const existing = await azJson<ManagedIdentityResponse>([
      "identity", "show", "--resource-group", rg, "--name", miName,
    ]);
    if (existing) return existing;
  } catch {
    // Falls through to create.
  }
  const created = await azJson<ManagedIdentityResponse>([
    "identity", "create",
    "--resource-group", rg,
    "--name", miName,
    "--location", region,
  ]);
  if (!created) throw new Error(`az identity create returned no output for ${miName}`);
  return created;
}

interface FicListResp {
  value: { id: string; name: string; subject: string }[];
}

async function ensureBlueprintMiAsFic(
  blueprintObjectId: string,
  tenantId: string,
  miPrincipalId: string,
): Promise<void> {
  const issuer = `https://login.microsoftonline.com/${tenantId}/v2.0`;
  const existing = await azGraphRest<FicListResp>(
    "GET",
    `/v1.0/applications/${blueprintObjectId}/federatedIdentityCredentials`,
  );
  if (
    existing &&
    existing.value &&
    existing.value.some((f) => f.subject === miPrincipalId)
  ) {
    return;
  }
  await azGraphRest(
    "POST",
    `/v1.0/applications/${blueprintObjectId}/federatedIdentityCredentials`,
    {
      name: "kars-controller-mi",
      issuer,
      subject: miPrincipalId,
      audiences: ["api://AzureADTokenExchange"],
    },
  );
}

async function writeKarsAuthConfig(result: {
  tenantId: string;
  blueprintClientId: string;
  blueprintObjectId: string;
  controllerMiClientId: string;
  controllerMiResourceId: string;
  controllerMiPrincipalId: string;
  serviceTree?: string;
}): Promise<void> {
  const cr: Record<string, unknown> = {
    apiVersion: "kars.azure.com/v1alpha1",
    kind: "KarsAuthConfig",
    metadata: { name: "default" },
    spec: {
      tenant: {
        tenantId: result.tenantId,
        authorityHost: "https://login.microsoftonline.com/",
        ...(result.serviceTree
          ? { serviceManagementReference: result.serviceTree }
          : {}),
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

  // `kubectl apply -f -` is the simplest portable way to write the CR.
  // Server-side apply would be slightly cleaner but requires the CRD
  // to already be installed; this works against any cluster where
  // the kars Helm chart has run.
  await execa("kubectl", ["apply", "-f", "-"], {
    input: JSON.stringify(cr),
    stdio: ["pipe", "inherit", "inherit"],
  });
}

/// Idempotent end-to-end auto-provision. Safe to call multiple times.
///
/// Returns the final IDs the rest of `kars up` needs (notably the
/// controller MI ARM resource ID, which `kars up` then assigns to the
/// AKS sandbox node pool VMSS).
export async function ensureAgentIdTrust(
  opts: AgentIdSetupOptions,
): Promise<AgentIdSetupResult> {
  const auth = await ensureAzAuth();
  const tenantId = opts.subscriptionId ? opts.subscriptionId : auth.tenantId; // tenant from az, NOT sub
  const realTenant = auth.tenantId;
  const subscriptionId = opts.subscriptionId ?? auth.subscriptionId;
  void tenantId; // tsc happy — placeholder until multi-tenant CLI support

  const clusterName = opts.clusterName ?? "kars";
  const rg = opts.resourceGroup ?? `${clusterName}-agentid-rg`;
  const region = opts.region ?? "eastus";
  const serviceTree =
    opts.serviceTree && opts.serviceTree.trim()
      ? opts.serviceTree.trim()
      : (process.env.KARS_SERVICE_TREE ?? "").trim() || undefined;
  const blueprintDisplayName = `kars-${clusterName}-blueprint`;
  const miName = `${clusterName}-controller-mi`;

  section("Entra Agent ID — auto-provision");
  kvLine("Tenant", realTenant);
  kvLine("Subscription", subscriptionId);
  kvLine("Signed in as", auth.user);
  kvLine("Blueprint display name", blueprintDisplayName);
  if (serviceTree) kvLine("Service tree GUID", serviceTree);

  if (opts.dryRun) {
    console.log(chalk.yellow("  ⚠ --dry-run: no changes were made."));
    return {
      tenantId: realTenant,
      blueprintClientId: "<dry-run>",
      blueprintObjectId: "<dry-run>",
      controllerMiClientId: "<dry-run>",
      controllerMiResourceId: "<dry-run>",
      controllerMiPrincipalId: "<dry-run>",
      freshlyCreated: false,
    };
  }

  // Phase 1: blueprint.
  let blueprint = await findExistingBlueprint(blueprintDisplayName);
  let freshlyCreated = false;
  if (!blueprint) {
    const userOid = await getCurrentUserOid();
    blueprint = await createBlueprint(blueprintDisplayName, userOid, serviceTree);
    freshlyCreated = true;
    kvLine("Blueprint", chalk.green(`created (appId=${blueprint.appId})`));
  } else {
    kvLine("Blueprint", chalk.dim(`reused (appId=${blueprint.appId})`));
  }

  // Phase 2: SP for blueprint.
  const sp = await ensureBlueprintSp(blueprint.appId);
  kvLine("Blueprint SP", chalk.dim(sp.id));

  // Phase 3: controller MI in customer sub.
  await ensureResourceGroup(rg, region);
  const mi = await ensureControllerMi(rg, region, miName);
  kvLine("Controller MI", chalk.dim(`${mi.clientId} (rg=${rg})`));

  // Phase 4: MI-as-FIC on blueprint.
  await ensureBlueprintMiAsFic(blueprint.id, realTenant, mi.principalId);
  kvLine("MI-as-FIC", chalk.green("present"));

  // Phase 5: KarsAuthConfig CR.
  // kubectl apply may fail if the CRD hasn't been installed yet
  // (e.g. when kars up runs this BEFORE Helm chart install). Caller
  // is responsible for invoking this in the right order; we surface
  // the error message so up.ts can decide whether to retry.
  try {
    await writeKarsAuthConfig({
      tenantId: realTenant,
      blueprintClientId: blueprint.appId,
      blueprintObjectId: blueprint.id,
      controllerMiClientId: mi.clientId,
      controllerMiResourceId: mi.id,
      controllerMiPrincipalId: mi.principalId,
      serviceTree,
    });
    kvLine("KarsAuthConfig CR", chalk.green("applied"));
  } catch (e) {
    const msg = (e as Error).message;
    if (msg.includes("no matches for kind")) {
      kvLine(
        "KarsAuthConfig CR",
        chalk.yellow("CRD not installed yet — caller should retry after Helm install"),
      );
    } else {
      throw e;
    }
  }

  return {
    tenantId: realTenant,
    blueprintClientId: blueprint.appId,
    blueprintObjectId: blueprint.id,
    controllerMiClientId: mi.clientId,
    controllerMiResourceId: mi.id,
    controllerMiPrincipalId: mi.principalId,
    freshlyCreated,
  };
}

/// Check whether `KarsAuthConfig/default` already exists in the
/// current kubeconfig context. Used by `kars up` to decide whether
/// auto-provisioning is needed at all.
export async function karsAuthConfigExists(): Promise<boolean> {
  try {
    const res = await execa(
      "kubectl",
      ["get", "karsauthconfig", "default", "-o", "name"],
      { stdio: "pipe" },
    );
    return res.stdout.includes("karsauthconfig/default");
  } catch {
    return false;
  }
}
