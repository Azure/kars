// Copyright (c) Microsoft Corporation.
// ci:loc-ok — Entra Agent ID feature module, split planned for Phase 1 (see ci/loc-budget.yaml)

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
  /// Cluster name (used to suffix the controller managed identity).
  /// Defaults to "kars". Multiple kars clusters in the same tenant
  /// share the same blueprint but each have their own controller MI.
  clusterName?: string;
  /// Tenant-wide blueprint display name. Defaults to "kars-blueprint".
  /// Override only when you want isolated blueprints per environment
  /// (e.g. one for prod, one for dev). All clusters using the same
  /// blueprint name will share governance (sponsors, owners).
  blueprintName?: string;
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
  /// Credential mode the auth-sidecar should be provisioned for:
  ///   - "auto" (default): try WorkloadIdentity first, fall back to
  ///     ManagedIdentityImds on `InvalidFederatedIdentityCredentialValue`
  ///     (the tenant rejected the AKS OIDC issuer).
  ///   - "WorkloadIdentity": OSS / non-restricted tenants. Creates
  ///     a SA-as-FIC pointing at the cluster's AKS OIDC issuer.
  ///     No controller MI is provisioned.
  ///   - "ManagedIdentityImds": corp-tenant safe. Creates a controller
  ///     MI and an MI-as-FIC on the blueprint. Current default for
  ///     existing kars deployments.
  credentialMode?: "auto" | "WorkloadIdentity" | "ManagedIdentityImds";
  /// AKS cluster name + RG for fetching the OIDC issuer URL in
  /// WorkloadIdentity / auto mode. When omitted in WI mode we read
  /// them from `kubectl config` and `az aks list` heuristically.
  aksClusterName?: string;
  aksClusterResourceGroup?: string;
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
  /// Which credential mode was actually provisioned. May differ from
  /// the requested mode when the caller passed `auto` and the tenant
  /// rejected the AKS OIDC issuer (we then fall back to
  /// `ManagedIdentityImds`).
  credentialMode: "WorkloadIdentity" | "ManagedIdentityImds";
  /// AKS OIDC issuer URL, present when `credentialMode=WorkloadIdentity`.
  aksOidcIssuerUrl?: string;
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
  return azGraphRestWithRetry<T>(method, graphPath, body, false);
}

/// Internal: shared implementation. `alreadyRetried` prevents an
/// infinite loop if device-code re-login also returns AADSTS530084.
async function azGraphRestWithRetry<T>(
  method: "GET" | "POST" | "PATCH" | "DELETE",
  graphPath: string,
  body: unknown,
  alreadyRetried: boolean,
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
    const msg = (err.stderr ?? err.message ?? "").toString();

    // AADSTS530084 = Conditional Access token-binding policy block on
    // the az CLI's first-party app token for Microsoft Graph. Trying
    // a device-code re-login refreshes the Graph token via a
    // different OAuth flow that some tenant CA policies treat
    // differently (the device-code app may not be subject to the
    // same token-binding rule). One-shot: if it fails again, give
    // up and propagate so the caller can fall back to Bicep.
    if (!alreadyRetried && msg.includes("AADSTS530084")) {
      const tenantArgs = await deviceCodeReloginForGraph();
      if (tenantArgs) {
        return azGraphRestWithRetry<T>(method, graphPath, body, true);
      }
    }

    throw new Error(msg.trim() || `az rest ${graphPath} failed`);
  }
}

/// Attempt a one-shot `az login --use-device-code --scope graph`
/// refresh of the Graph token cache. Returns true on success so the
/// caller can retry the failing Graph call.
///
/// Device code flow uses a different OAuth path than the default
/// interactive flow — the user authenticates on a SECOND device by
/// visiting microsoft.com/devicelogin and entering the code. Some
/// Conditional Access policies that block token-binding on the
/// primary device's az CLI session do not apply to this flow.
async function deviceCodeReloginForGraph(): Promise<boolean> {
  console.log();
  console.log(
    "  ↻ AADSTS530084: az CLI token is CA-blocked for Microsoft Graph.",
  );
  console.log(
    "    Attempting device-code re-login for the Graph scope...",
  );
  console.log(
    "    You will be prompted to visit https://microsoft.com/devicelogin in a browser.",
  );

  try {
    await execa(
      "az",
      [
        "login",
        "--use-device-code",
        "--scope",
        "https://graph.microsoft.com//.default",
        "--allow-no-subscriptions",
      ],
      // Use inherit so the device-code prompt is visible to the user.
      { stdio: ["inherit", "inherit", "inherit"] },
    );
    console.log("  ✓ Graph token cache refreshed via device-code flow.");
    return true;
  } catch (e) {
    const msg = (e as { stderr?: string; message?: string }).stderr ?? (e as Error).message ?? "";
    console.log(
      `  ✘ Device-code re-login also failed (${msg.split("\n")[0].slice(0, 120)}).`,
    );
    console.log("    Will fall back to Bicep ARM deployment.");
    return false;
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
  const me = await azGraphRest<MeApiResponse>("GET", "/beta/me");
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
    `/beta/applications?$filter=${filter}&$top=2`,
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
  // Body shape matches the user-verified working request in Graph
  // Explorer:
  //   - @odata.type WITHOUT the `#` prefix (Graph accepts both, but
  //     this form is what the Entra Agents portal docs publish).
  //   - sponsors/owners @odata.bind URLs use /v1.0/users/ specifically
  //     (Graph rejects /beta/ in odata.bind refs from the Application
  //     namespace — the resource URL must be the v1.0 entity, even
  //     when the parent POST URL is /beta).
  const body: Record<string, unknown> = {
    "@odata.type": "Microsoft.Graph.AgentIdentityBlueprint",
    displayName,
    "sponsors@odata.bind": [`https://graph.microsoft.com/v1.0/users/${userOid}`],
    "owners@odata.bind": [`https://graph.microsoft.com/v1.0/users/${userOid}`],
  };
  if (serviceTree && serviceTree.trim()) {
    body.serviceManagementReference = serviceTree.trim();
  }

  const created = await azGraphRest<BlueprintGraphResponse>(
    "POST",
    "/beta/applications/",
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
    `/beta/servicePrincipals?$filter=${filter}&$top=1`,
  );
  if (existing && existing.value && existing.value.length > 0) {
    return existing.value[0];
  }
  // Create.
  const created = await azGraphRest<SpGraphResponse>(
    "POST",
    "/beta/servicePrincipals",
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
    `/beta/applications/${blueprintObjectId}/federatedIdentityCredentials`,
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
    `/beta/applications/${blueprintObjectId}/federatedIdentityCredentials`,
    {
      name: "kars-controller-mi",
      issuer,
      subject: miPrincipalId,
      audiences: ["api://AzureADTokenExchange"],
    },
  );
}

/// Sentinel error class thrown when the tenant's issuer-allowlist
/// policy rejects the AKS OIDC issuer (Microsoft corporate tenant
/// observed `InvalidFederatedIdentityCredentialValue`). The auto-mode
/// orchestrator catches this and falls back to ManagedIdentityImds.
class TenantRejectedAksOidcIssuer extends Error {
  constructor(public readonly issuer: string, cause: string) {
    super(
      `Tenant rejected AKS OIDC issuer '${issuer}' as a federated identity credential — ${cause}`,
    );
    this.name = "TenantRejectedAksOidcIssuer";
  }
}

/// Create (or verify) a SA-as-FIC on the blueprint with the given
/// AKS OIDC issuer URL and SA subject. Used in Pattern B.
/// Throws `TenantRejectedAksOidcIssuer` on
/// `InvalidFederatedIdentityCredentialValue` so the auto-mode
/// orchestrator can recognise the failure cleanly.
async function ensureBlueprintSaAsFic(
  blueprintObjectId: string,
  aksOidcIssuerUrl: string,
  saNamespace: string,
  saName: string,
): Promise<void> {
  const subject = `system:serviceaccount:${saNamespace}:${saName}`;
  const existing = await azGraphRest<FicListResp>(
    "GET",
    `/beta/applications/${blueprintObjectId}/federatedIdentityCredentials`,
  );
  if (
    existing &&
    existing.value &&
    existing.value.some(
      (f) => f.subject === subject && (f as { issuer?: string }).issuer === aksOidcIssuerUrl,
    )
  ) {
    return;
  }
  try {
    await azGraphRest(
      "POST",
      `/beta/applications/${blueprintObjectId}/federatedIdentityCredentials`,
      {
        name: "kars-auth-sidecar-sa",
        issuer: aksOidcIssuerUrl,
        subject,
        audiences: ["api://AzureADTokenExchange"],
      },
    );
  } catch (e) {
    const msg = (e as Error).message ?? "";
    // Microsoft-corporate tenants set an `InvalidFederatedIdentityCredentialValue`
    // policy that rejects AKS-OIDC-shaped issuers — the documented
    // marker substring is `not allowed as per assigned policy`.
    if (
      msg.includes("InvalidFederatedIdentityCredentialValue") ||
      msg.includes("not allowed as per assigned policy")
    ) {
      throw new TenantRejectedAksOidcIssuer(aksOidcIssuerUrl, msg.split("\n")[0]);
    }
    throw e;
  }
}

interface AksClusterShowResponse {
  name: string;
  resourceGroup: string;
  oidcIssuerProfile?: { enabled?: boolean; issuerUrl?: string };
}

/// Look up the AKS cluster's OIDC issuer URL, required for Pattern B.
///
/// When the caller passed explicit `aksClusterName + aksClusterResourceGroup`,
/// uses those. Otherwise discovers the kubeconfig current-context's
/// cluster name and walks `az aks list` to find an AKS cluster matching
/// it. Returns `null` if no matching AKS cluster is discoverable — the
/// caller then falls back to Pattern A.
async function discoverAksOidcIssuerUrl(opts: {
  aksClusterName?: string;
  aksClusterResourceGroup?: string;
}): Promise<{ name: string; rg: string; issuerUrl: string } | null> {
  const fromArgs =
    opts.aksClusterName && opts.aksClusterResourceGroup
      ? { name: opts.aksClusterName, rg: opts.aksClusterResourceGroup }
      : await guessAksFromKubeconfig();
  if (!fromArgs) return null;

  const show = await azJson<AksClusterShowResponse>([
    "aks",
    "show",
    "--name",
    fromArgs.name,
    "--resource-group",
    fromArgs.rg,
  ]);
  if (!show) return null;
  const url = show.oidcIssuerProfile?.issuerUrl;
  if (!url) {
    throw new Error(
      `AKS cluster ${fromArgs.rg}/${fromArgs.name} does not have an OIDC issuer URL. ` +
        `Enable it with: az aks update -n ${fromArgs.name} -g ${fromArgs.rg} --enable-oidc-issuer`,
    );
  }
  return { name: fromArgs.name, rg: fromArgs.rg, issuerUrl: url };
}

interface KubeconfigDoc {
  "current-context": string;
  contexts: { name: string; context: { cluster: string } }[];
}

async function guessAksFromKubeconfig(): Promise<{ name: string; rg: string } | null> {
  let cluster: string | null = null;
  try {
    const res = await execa("kubectl", ["config", "view", "-o", "json"], {
      stdio: ["ignore", "pipe", "pipe"],
    });
    const doc = JSON.parse(res.stdout) as KubeconfigDoc;
    const current = doc["current-context"];
    const ctx = doc.contexts.find((c) => c.name === current);
    cluster = ctx?.context.cluster ?? null;
  } catch {
    return null;
  }
  if (!cluster) return null;
  // kubectl + AKS conventionally name the cluster after the AKS
  // resource name. Match the first AKS resource with that name.
  const list = await azJson<{ name: string; resourceGroup: string }[]>([
    "aks",
    "list",
    "--query",
    `[?name=='${cluster}']`,
  ]);
  if (!list || list.length === 0) return null;
  return { name: list[0].name, rg: list[0].resourceGroup };
}

async function writeKarsAuthConfig(result: {
  tenantId: string;
  blueprintClientId: string;
  blueprintObjectId: string;
  controllerMiClientId?: string;
  controllerMiResourceId?: string;
  controllerMiPrincipalId?: string;
  credentialMode: "WorkloadIdentity" | "ManagedIdentityImds";
  serviceTree?: string;
}): Promise<void> {
  // MI fields are populated in Pattern A and stripped in Pattern B —
  // the controller-side validator (auth_config_reconciler) refuses
  // to materialise an MI-mode CR with empty MI clientId, so we must
  // not write blank strings in WI mode (would falsely register as
  // "populated" to the validator's is-non-empty check).
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
      // Phase 6.b: verified-tier mesh peer authentication.
      // Audience is the blueprint clientId because the blueprint SP
      // is the only Entra resource we're guaranteed exists in every
      // customer tenant — `api://agentmesh` is a kars-Azure-internal
      // SP that customer tenants don't have. The token's `azp` claim
      // carries the per-sandbox agent identity appId, which the
      // registry stamps as `verified_app_id`.
      //
      // Enabled here unconditionally because this `writeKarsAuthConfig`
      // is only called from `--mesh-trust entra` (or explicit
      // `kars mesh setup-trust`); the operator has already opted in.
      // Anonymous-tier deployments never reach this code path
      // because `--mesh-trust anonymous` (the default) skips the
      // Entra Agent ID block entirely.
      meshAuthBackend: "EntraAgentIdentity",
      meshAuthAudience: `${result.blueprintClientId}/.default`,
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
  void tenantId; // ci:stub-ok: tsc-only no-op; multi-tenant CLI ships in Phase 1

  const clusterName = opts.clusterName ?? "kars";
  const rg = opts.resourceGroup ?? `${clusterName}-agentid-rg`;
  const region = opts.region ?? "eastus";
  const serviceTree =
    opts.serviceTree && opts.serviceTree.trim()
      ? opts.serviceTree.trim()
      : (process.env.KARS_SERVICE_TREE ?? "").trim() || undefined;
  const blueprintDisplayName =
    opts.blueprintName && opts.blueprintName.trim()
      ? opts.blueprintName.trim()
      : "kars-blueprint";
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
      credentialMode: opts.credentialMode === "WorkloadIdentity" ? "WorkloadIdentity" : "ManagedIdentityImds",
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

  // Phase 3-4: Credential-mode-dependent provisioning.
  //
  // For Pattern A (ManagedIdentityImds): create controller MI + MI-as-FIC.
  // For Pattern B (WorkloadIdentity):    discover AKS OIDC + create SA-as-FIC.
  // For "auto":                          try B first, fall back to A on
  //                                      InvalidFederatedIdentityCredentialValue.
  const requestedMode = opts.credentialMode ?? "auto";
  let resolvedMode: "WorkloadIdentity" | "ManagedIdentityImds";
  let mi: ManagedIdentityResponse | null = null;
  let aksOidcIssuerUrl: string | undefined;

  const tryPatternB = async (): Promise<boolean> => {
    const aks = await discoverAksOidcIssuerUrl({
      aksClusterName: opts.aksClusterName,
      aksClusterResourceGroup: opts.aksClusterResourceGroup,
    });
    if (!aks) {
      kvLine(
        "AKS OIDC issuer",
        chalk.yellow("not discoverable — cannot try WorkloadIdentity"),
      );
      return false;
    }
    kvLine("AKS OIDC issuer", chalk.dim(aks.issuerUrl));
    try {
      await ensureBlueprintSaAsFic(
        blueprint.id,
        aks.issuerUrl,
        "kars-system",
        "entra-auth-sidecar",
      );
      kvLine("SA-as-FIC", chalk.green("present"));
      aksOidcIssuerUrl = aks.issuerUrl;
      return true;
    } catch (e) {
      if (e instanceof TenantRejectedAksOidcIssuer) {
        kvLine(
          "SA-as-FIC",
          chalk.yellow("tenant rejected AKS OIDC issuer — falling back to ManagedIdentityImds"),
        );
        return false;
      }
      throw e;
    }
  };

  if (requestedMode === "WorkloadIdentity") {
    const ok = await tryPatternB();
    if (!ok) {
      throw new Error(
        "credentialMode=WorkloadIdentity requested but Pattern B provisioning failed. " +
          "Either fix the AKS OIDC issuer configuration or use --credential-mode ManagedIdentityImds.",
      );
    }
    resolvedMode = "WorkloadIdentity";
  } else if (requestedMode === "auto") {
    const ok = await tryPatternB();
    if (ok) {
      resolvedMode = "WorkloadIdentity";
    } else {
      await ensureResourceGroup(rg, region);
      mi = await ensureControllerMi(rg, region, miName);
      kvLine("Controller MI", chalk.dim(`${mi.clientId} (rg=${rg})`));
      await ensureBlueprintMiAsFic(blueprint.id, realTenant, mi.principalId);
      kvLine("MI-as-FIC", chalk.green("present"));
      resolvedMode = "ManagedIdentityImds";
    }
  } else {
    // requestedMode === "ManagedIdentityImds"
    await ensureResourceGroup(rg, region);
    mi = await ensureControllerMi(rg, region, miName);
    kvLine("Controller MI", chalk.dim(`${mi.clientId} (rg=${rg})`));
    await ensureBlueprintMiAsFic(blueprint.id, realTenant, mi.principalId);
    kvLine("MI-as-FIC", chalk.green("present"));
    resolvedMode = "ManagedIdentityImds";
  }

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
      controllerMiClientId: mi?.clientId,
      controllerMiResourceId: mi?.id,
      controllerMiPrincipalId: mi?.principalId,
      credentialMode: resolvedMode,
      serviceTree,
    });
    kvLine("KarsAuthConfig CR", chalk.green(`applied (mode=${resolvedMode})`));
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
    controllerMiClientId: mi?.clientId ?? "",
    controllerMiResourceId: mi?.id ?? "",
    controllerMiPrincipalId: mi?.principalId ?? "",
    credentialMode: resolvedMode,
    aksOidcIssuerUrl,
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
    // kubectl may return either short form ("karsauthconfig/default")
    // or fully-qualified ("karsauthconfig.kars.azure.com/default")
    // depending on cluster version; match the trailing "/default"
    // segment which is invariant.
    return res.stdout.includes("/default");
  } catch {
    return false;
  }
}

/// Check that the signed-in user has the Entra `Agent ID Developer`
/// directory role (or one of the stronger roles that supersede it:
/// `Agent ID Administrator`, `Privileged Role Administrator`,
/// `Global Administrator`). Required to provision agent identity
/// blueprints via Graph.
///
/// Returns the result rather than throwing so callers (notably
/// `cli/src/preflight.ts`) can include it in their aggregated
/// preflight summary. We deliberately treat "Graph lookup failed"
/// as a soft pass with a warning, mirroring the behaviour of other
/// preflight checks that can't fully evaluate due to permission
/// transients.
export interface AgentIdRoleCheckResult {
  /// `true` when at least one supporting role assignment was
  /// detected. `false` when the user is definitively missing the
  /// role.
  hasRole: boolean;
  /// `true` when the check could not run conclusively (e.g. the
  /// /me/memberOf call was rate-limited). Callers should surface
  /// as a warning rather than blocking.
  inconclusive: boolean;
  /// Human-readable diagnostic. Always populated.
  message: string;
  /// Object IDs of the role assignments detected, for diagnostics.
  detectedRoles: { id: string; displayName: string }[];
}

interface MeMemberOfResp {
  value: Array<{
    "@odata.type"?: string;
    id: string;
    displayName?: string;
    roleTemplateId?: string;
  }>;
}

const AGENT_ID_ROLE_TEMPLATE_IDS: Record<string, string> = {
  // Pinned from Entra ID documentation. If Microsoft renames, the
  // displayName fallback below catches it.
  "Agent ID Developer": "8424c6f0-a189-499e-bbd0-26c1753c96d4",
  "Agent ID Administrator": "82e2c3d5-19d8-486a-a3f5-d70e000ed05f",
  "Privileged Role Administrator": "e8611ab8-c189-46e8-94e1-60213ab1f814",
  "Global Administrator": "62e90394-69f5-4237-9190-012177145e10",
};

/// Check whether the signed-in user holds at least one of the roles
/// that permit blueprint creation. Calls Microsoft Graph
/// `/me/transitiveMemberOf`. Soft-fails on permission errors —
/// returning `inconclusive: true` so preflight surfaces a warning,
/// not a block.
export async function checkAgentIdRole(): Promise<AgentIdRoleCheckResult> {
  let resp: MeMemberOfResp | null;
  try {
    resp = await azGraphRest<MeMemberOfResp>(
      "GET",
      "/beta/me/transitiveMemberOf/microsoft.graph.directoryRole?$select=id,displayName,roleTemplateId&$top=100",
    );
  } catch (e) {
    const msg = (e as Error).message;
    // AADSTS530084 = Conditional Access token-binding block. Common in
    // Microsoft-corporate tenants where the Azure CLI's first-party
    // token cache hasn't been refreshed for Microsoft Graph. The
    // mitigation is documented; surface it concretely instead of
    // generic "could not enumerate".
    if (msg.includes("AADSTS530084")) {
      return {
        hasRole: false,
        inconclusive: true,
        message:
          "Azure CLI token is Conditional-Access-blocked for Microsoft Graph (AADSTS530084). " +
          "Run: az login --scope https://graph.microsoft.com//.default — then retry. " +
          "See docs/agent-identity.md#az-cli-ca-block.",
        detectedRoles: [],
      };
    }
    // AADSTS65001 / AADSTS65002 = missing first-party app consent for
    // Graph. Mitigation is the same `az login --scope` retry.
    if (msg.includes("AADSTS65001") || msg.includes("AADSTS65002")) {
      return {
        hasRole: false,
        inconclusive: true,
        message:
          "Azure CLI lacks Microsoft Graph consent for this session. " +
          "Run: az login --scope https://graph.microsoft.com//.default — then retry.",
        detectedRoles: [],
      };
    }
    return {
      hasRole: false,
      inconclusive: true,
      message: `Could not enumerate directory roles (${msg.split("\n")[0].slice(0, 120)})`,
      detectedRoles: [],
    };
  }

  const assignments = resp?.value ?? [];
  const supportedTemplateIds = new Set(Object.values(AGENT_ID_ROLE_TEMPLATE_IDS));
  const supportedNames = new Set(Object.keys(AGENT_ID_ROLE_TEMPLATE_IDS));

  const detected = assignments
    .filter((r) =>
      // Match by either the role template id (stable) or display
      // name (handy if Microsoft adds new role variants).
      (r.roleTemplateId && supportedTemplateIds.has(r.roleTemplateId)) ||
      (r.displayName && supportedNames.has(r.displayName)),
    )
    .map((r) => ({ id: r.id, displayName: r.displayName ?? "<unknown>" }));

  if (detected.length === 0) {
    return {
      hasRole: false,
      inconclusive: false,
      message:
        "Signed-in user has no Agent ID-capable Entra role. Required: 'Agent ID Developer' (or stronger).",
      detectedRoles: [],
    };
  }

  return {
    hasRole: true,
    inconclusive: false,
    message: `Detected ${detected.length} matching role assignment(s): ${detected
      .map((d) => d.displayName)
      .join(", ")}`,
    detectedRoles: detected,
  };
}

/// Best-effort check that the tenant has any agent-identity
/// blueprints already provisioned. Used by preflight to detect
/// whether a previous `kars` run wired up the blueprint — informs
/// the "skip vs run setup-trust" decision without actually
/// calling `karsAuthConfigExists` (which depends on a working
/// kubeconfig that may not be ready yet during preflight).
export async function detectExistingBlueprint(
  displayName: string,
): Promise<{ present: boolean; appId?: string; message: string }> {
  try {
    const blueprint = await findExistingBlueprint(displayName);
    if (blueprint) {
      return {
        present: true,
        appId: blueprint.appId,
        message: `Blueprint '${displayName}' already exists (appId=${blueprint.appId})`,
      };
    }
    return {
      present: false,
      message: `No existing '${displayName}' blueprint — will be created by kars up`,
    };
  } catch (e) {
    const msg = (e as Error).message;
    return {
      present: false,
      message: `Graph lookup inconclusive (${msg.split("\n")[0].slice(0, 100)})`,
    };
  }
}

/// Smart auto-provision: try the fast Graph REST path first, and
/// transparently fall back to the Bicep ARM path when the Microsoft
/// corporate tenant Conditional Access policy blocks the Azure CLI's
/// Graph token (AADSTS530084) — common in policed enterprise tenants.
///
/// `kars up` calls this wrapper. The CLI flag
/// `kars mesh setup-trust --mode agent-id` also uses it. Operators
/// who want to skip the CLI attempt entirely (e.g. they know their
/// tenant always blocks CLI Graph) can run
/// `kars mesh setup-trust --mode bicep` directly.
///
/// Returns the same shape as `ensureAgentIdTrust`. The `freshlyCreated`
/// field is preserved across modes.
export async function ensureAgentIdTrustAutoFallback(
  opts: AgentIdSetupOptions,
): Promise<AgentIdSetupResult> {
  try {
    return await ensureAgentIdTrust(opts);
  } catch (e) {
    const msg = (e as Error).message;
    if (!msg.includes("AADSTS530084")) {
      // Not the CA-block error — propagate unchanged. Avoids surprising
      // users with a Bicep attempt for genuine permission or config
      // errors that Bicep would also fail on.
      throw e;
    }

    console.log();
    console.log(
      `  ↻ Graph REST blocked by tenant Conditional Access policy (AADSTS530084).`,
    );
    console.log(
      `    Falling back to Bicep ARM deployment — bypasses az CLI Graph CA.`,
    );

    // The Bicep path doesn't have its own auto-mode (Bicep is
    // declarative — it can't try a FIC and fall back). When the
    // imperative path failed at the AADSTS530084 stage we don't yet
    // know whether the tenant accepts AKS OIDC. Conservative choice:
    // run Bicep in `ManagedIdentityImds` mode (the same mode that
    // worked historically in CA-blocked tenants). Operators who want
    // Pattern B via the Bicep path can re-run with
    // `--credential-mode WorkloadIdentity` + `--aks-oidc-issuer-url`.
    const explicitMode = opts.credentialMode;
    const bicepMode: "ManagedIdentityImds" | "WorkloadIdentity" =
      explicitMode === "WorkloadIdentity"
        ? "WorkloadIdentity"
        : "ManagedIdentityImds";
    let aksOidcIssuerUrl: string | undefined;
    if (bicepMode === "WorkloadIdentity") {
      const aks = await discoverAksOidcIssuerUrl({
        aksClusterName: opts.aksClusterName,
        aksClusterResourceGroup: opts.aksClusterResourceGroup,
      });
      aksOidcIssuerUrl = aks?.issuerUrl;
    }

    const { ensureAgentIdTrustViaBicep } = await import("./agent_id_setup_bicep.js");
    const bicepResult = await ensureAgentIdTrustViaBicep({
      clusterName: opts.clusterName,
      resourceGroup: opts.resourceGroup,
      region: opts.region ?? "eastus",
      serviceTree: opts.serviceTree,
      credentialMode: bicepMode,
      aksOidcIssuerUrl,
      dryRun: opts.dryRun,
    });

    // Map BicepSetupResult → AgentIdSetupResult. The bicep path
    // always upserts (idempotent) so we conservatively claim
    // freshlyCreated=true when we go through this fallback — operators
    // who care about the distinction can inspect az deployment history.
    return {
      ...bicepResult,
      freshlyCreated: true,
    };
  }
}
