// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// `kars mesh setup-trust` — provisions the Entra ID app registration
// and service principal that kars sandboxes use to acquire access
// tokens with audience `api://agentmesh/.default`. Without this, sandbox
// pods fall back to the AGT anonymous tier (trust score 0) and every
// peer KNOCK is gated against that floor.
//
// This is a tenant-wide, one-time operation and requires Application
// Administrator (or higher). The CLI is idempotent: if the app reg
// already exists with the right identifier URI, we just print the IDs
// and exit cleanly.
//
// Concrete az calls equivalent to what this command runs:
//
//   az ad app create \
//     --display-name "<display-name>" \
//     --identifier-uris "api://agentmesh" \
//     --sign-in-audience AzureADMyOrg
//   az ad sp create --id <app-id>

import chalk from "chalk";
import { Command } from "commander";
import { execa } from "execa";
import { banner, section, kvLine } from "../../stepper.js";

const AGENTMESH_IDENTIFIER_URI = "api://agentmesh";

interface AppRegistration {
  appId: string;
  id: string; // object id
  displayName: string;
  identifierUris: string[];
  signInAudience: string;
}

interface ServicePrincipal {
  id: string;
  appId: string;
  displayName: string;
}

async function azJson<T>(args: string[]): Promise<T | null> {
  try {
    const res = await execa("az", [...args, "-o", "json"], { stdio: ["ignore", "pipe", "pipe"] });
    if (!res.stdout || res.stdout.trim() === "" || res.stdout.trim() === "null") return null;
    return JSON.parse(res.stdout) as T;
  } catch (e: unknown) {
    const err = e as { stderr?: string; message?: string; exitCode?: number };
    const stderr = err.stderr ?? err.message ?? "";
    throw new Error(stderr.trim() || `az ${args.join(" ")} failed`);
  }
}

async function ensureAzAuth(): Promise<{ tenantId: string; subscription: string; user: string }> {
  let account: { tenantId: string; id: string; user: { name: string } } | null;
  try {
    account = await azJson<typeof account>(["account", "show"]);
  } catch {
    throw new Error("Azure CLI is not signed in — run `az login` first.");
  }
  if (!account || !account.tenantId) {
    throw new Error("Azure CLI is not signed in — run `az login` first.");
  }
  return { tenantId: account.tenantId, subscription: account.id, user: account.user?.name ?? "<unknown>" };
}

async function findExistingApp(): Promise<AppRegistration | null> {
  const apps = await azJson<AppRegistration[]>([
    "ad", "app", "list",
    "--identifier-uri", AGENTMESH_IDENTIFIER_URI,
  ]);
  if (!apps || apps.length === 0) return null;
  return apps[0];
}

async function findServicePrincipal(appId: string): Promise<ServicePrincipal | null> {
  const sps = await azJson<ServicePrincipal[]>([
    "ad", "sp", "list",
    "--filter", `appId eq '${appId}'`,
  ]);
  if (!sps || sps.length === 0) return null;
  return sps[0];
}

async function createApp(displayName: string): Promise<AppRegistration> {
  const app = await azJson<AppRegistration>([
    "ad", "app", "create",
    "--display-name", displayName,
    "--identifier-uris", AGENTMESH_IDENTIFIER_URI,
    "--sign-in-audience", "AzureADMyOrg",
  ]);
  if (!app) throw new Error("az ad app create returned no output");
  return app;
}

async function createServicePrincipal(appId: string): Promise<ServicePrincipal> {
  const sp = await azJson<ServicePrincipal>([
    "ad", "sp", "create",
    "--id", appId,
  ]);
  if (!sp) throw new Error("az ad sp create returned no output");
  return sp;
}

export function attachSetupTrustSubcommand(cmd: Command): void {
  cmd
    .command("setup-trust")
    .description(
      `Provision the tenant Entra trust for kars sandboxes. Default (--mode agent-id) creates the Entra Agent ID blueprint + controller MI; --mode legacy provisions the deprecated ${AGENTMESH_IDENTIFIER_URI} app registration.`,
    )
    .option("--mode <mode>", "Trust mode: 'agent-id' (CLI/Graph), 'bicep' (ARM/Graph extension, bypasses az CLI Graph CA blocks), or 'legacy' (deprecated api://agentmesh)", "agent-id")
    .option("--display-name <name>", "Display name for the Entra app registration (legacy mode only)", "kars AgentMesh")
    .option("--service-tree <guid>", "ServiceTree / service-management-reference GUID (agent-id mode in Microsoft-style tenants)")
    .option("--cluster-name <name>", "Cluster name suffix for the controller MI (agent-id mode)", "kars")
    .option("--resource-group <name>", "Resource group for the controller MI (agent-id mode)")
    .option("--region <region>", "Azure region for the controller MI (agent-id mode)", "eastus")
    .option(
      "--credential-mode <mode>",
      "Auth-sidecar credential mode: 'auto' (try WorkloadIdentity first, fall back on InvalidFederatedIdentityCredentialValue), 'WorkloadIdentity' (require AKS-OIDC FIC), or 'ManagedIdentityImds' (force controller MI path). Defaults to 'auto'.",
      "auto",
    )
    .option(
      "--aks-cluster-name <name>",
      "AKS cluster name (used to discover the OIDC issuer URL when credentialMode is auto or WorkloadIdentity)",
    )
    .option(
      "--aks-cluster-resource-group <rg>",
      "AKS cluster resource group (used to discover the OIDC issuer URL when credentialMode is auto or WorkloadIdentity)",
    )
    .option("--aks-oidc-issuer-url <url>", "AKS OIDC issuer URL — direct override for Bicep mode + WorkloadIdentity")
    .option("--dry-run", "Print what would be created without making changes", false)
    .action(async (opts: {
      mode: string;
      displayName: string;
      serviceTree?: string;
      clusterName?: string;
      resourceGroup?: string;
      region?: string;
      credentialMode: "auto" | "WorkloadIdentity" | "ManagedIdentityImds";
      aksClusterName?: string;
      aksClusterResourceGroup?: string;
      aksOidcIssuerUrl?: string;
      dryRun: boolean;
    }) => {
      // ── Agent ID mode (recommended) ────────────────────────────
      // Forwards to the same idempotent helper that `kars up` invokes
      // automatically. Auto-fallback wrapper: tries the fast Graph
      // REST path first, transparently switches to Bicep when
      // Microsoft tenant Conditional Access blocks the az CLI's
      // Graph token (AADSTS530084). Use --mode bicep explicitly to
      // skip the CLI attempt entirely.
      if (opts.mode === "agent-id") {
        banner("kars · Mesh Setup Trust", "Entra Agent ID blueprint + controller MI");
        const { ensureAgentIdTrustAutoFallback, karsAuthConfigExists } = await import(
          "./agent_id_setup.js"
        );

        // Short-circuit when the cluster already has the CR — avoids
        // a wasteful Graph REST attempt (which always triggers a
        // device-code prompt in CA-blocked tenants) when the work is
        // already done. Operators who want to FORCE re-provisioning
        // (e.g. after manually deleting the blueprint) should delete
        // the CR first: `kubectl delete karsauthconfig default`.
        try {
          const exists = await karsAuthConfigExists();
          if (exists) {
            console.log();
            console.log(chalk.green("  ✓ KarsAuthConfig/default already present — trust is already provisioned."));
            console.log(chalk.dim("    Inspect:  kubectl get karsauthconfig default -o yaml"));
            console.log(chalk.dim("    Re-do:    kubectl delete karsauthconfig default && kars mesh setup-trust --mode agent-id"));
            return;
          }
        } catch {
          // kubectl not configured / cluster unreachable — fall through
          // and let the provisioning path produce a clearer error.
        }

        try {
          const result = await ensureAgentIdTrustAutoFallback({
            clusterName: opts.clusterName,
            resourceGroup: opts.resourceGroup,
            region: opts.region,
            serviceTree: opts.serviceTree,
            credentialMode: opts.credentialMode,
            aksClusterName: opts.aksClusterName,
            aksClusterResourceGroup: opts.aksClusterResourceGroup,
            dryRun: opts.dryRun,
          });
          console.log();
          console.log(chalk.green("  ✓ Entra Agent ID trust ready"));
          console.log(chalk.dim(`    blueprint client ID: ${result.blueprintClientId}`));
          console.log(chalk.dim(`    credential mode:     ${result.credentialMode}`));
          if (result.credentialMode === "ManagedIdentityImds" && result.controllerMiClientId) {
            console.log(chalk.dim(`    controller MI:       ${result.controllerMiClientId}`));
          }
          if (result.credentialMode === "WorkloadIdentity" && result.aksOidcIssuerUrl) {
            console.log(chalk.dim(`    AKS OIDC issuer:     ${result.aksOidcIssuerUrl}`));
          }
          console.log(chalk.dim(`    KarsAuthConfig:      kubectl get karsauthconfig default`));
        } catch (e) {
          const msg = (e as Error).message;
          console.error(chalk.red(`\n  ✘ ${msg.split("\n")[0]}`));
          if (msg.includes("Agent ID Developer") || msg.includes("Insufficient privileges")) {
            console.error(chalk.dim("\n    The signed-in identity needs the 'Agent ID Developer' Entra directory role."));
            console.error(chalk.dim("    Activate via PIM at https://portal.azure.com and retry."));
          }
          process.exit(1);
        }
        return;
      }

      // ── Bicep mode (CA-policy-tolerant) ────────────────────────
      // Runs the same provisioning as agent-id mode but via the
      // Microsoft.Graph Bicep extension. ARM's deployment principal
      // gets a fresh Graph token through its own auth path — bypasses
      // Conditional Access token-binding policy on the az CLI's
      // first-party app. Use this when `kars mesh setup-trust --mode
      // agent-id` fails with AADSTS530084.
      if (opts.mode === "bicep") {
        banner("kars · Mesh Setup Trust (bicep)", "Entra Agent ID via ARM deployment");
        const { ensureAgentIdTrustViaBicep } = await import("./agent_id_setup_bicep.js");
        const { karsAuthConfigExists } = await import("./agent_id_setup.js");

        // Short-circuit when the cluster already has the CR — Bicep
        // is idempotent but still consumes ~30-90s of ARM polling.
        // No reason to incur that cost when the trust is already in
        // place.
        try {
          const exists = await karsAuthConfigExists();
          if (exists) {
            console.log();
            console.log(chalk.green("  ✓ KarsAuthConfig/default already present — trust is already provisioned."));
            console.log(chalk.dim("    Inspect:  kubectl get karsauthconfig default -o yaml"));
            console.log(chalk.dim("    Re-do:    kubectl delete karsauthconfig default && kars mesh setup-trust --mode bicep"));
            return;
          }
        } catch {
          // kubectl not configured — fall through to provisioning.
        }

        try {
          const result = await ensureAgentIdTrustViaBicep({
            clusterName: opts.clusterName,
            resourceGroup: opts.resourceGroup,
            region: opts.region ?? "eastus",
            serviceTree: opts.serviceTree,
            credentialMode:
              opts.credentialMode === "WorkloadIdentity"
                ? "WorkloadIdentity"
                : "ManagedIdentityImds",
            aksOidcIssuerUrl: opts.aksOidcIssuerUrl,
            dryRun: opts.dryRun,
          });
          console.log();
          console.log(chalk.green("  ✓ Entra Agent ID trust ready (via Bicep)"));
          console.log(chalk.dim(`    blueprint client ID: ${result.blueprintClientId}`));
          console.log(chalk.dim(`    credential mode:     ${result.credentialMode}`));
          if (result.credentialMode === "ManagedIdentityImds" && result.controllerMiClientId) {
            console.log(chalk.dim(`    controller MI:       ${result.controllerMiClientId}`));
          }
          if (result.credentialMode === "WorkloadIdentity" && result.aksOidcIssuerUrl) {
            console.log(chalk.dim(`    AKS OIDC issuer:     ${result.aksOidcIssuerUrl}`));
          }
          console.log(chalk.dim(`    KarsAuthConfig:      kubectl get karsauthconfig default`));
        } catch (e) {
          const msg = (e as Error).message;
          console.error(chalk.red(`\n  ✘ ${msg.split("\n")[0]}`));
          process.exit(1);
        }
        return;
      }

      // ── Legacy mode ────────────────────────────────────────────
      // Preserves the original api://agentmesh flow for installations
      // that haven't migrated to Entra Agent ID yet. Slated for
      // removal once all consumers have switched.
      banner("kars · Mesh Setup Trust (legacy)", "Entra App Registration for api://agentmesh");

      // Step 1: confirm az is signed in and which tenant we're targeting
      section("Tenant");
      let tenant: Awaited<ReturnType<typeof ensureAzAuth>>;
      try {
        tenant = await ensureAzAuth();
      } catch (e) {
        console.error(chalk.red(`  ✘ ${(e as Error).message}`));
        process.exit(1);
      }
      kvLine("Tenant ID", tenant.tenantId);
      kvLine("Subscription", tenant.subscription);
      kvLine("Signed in as", tenant.user);

      // Step 2: idempotency check
      section("App registration");
      let existing: AppRegistration | null;
      try {
        existing = await findExistingApp();
      } catch (e) {
        const msg = (e as Error).message;
        console.error(chalk.red(`  ✘ ${msg}`));
        if (msg.includes("AADSTS530084") || msg.includes("conditional access")) {
          console.error(chalk.dim("    Conditional-access policy is blocking the Graph token this session has."));
          console.error(chalk.dim("    Re-authenticate explicitly for Microsoft Graph and retry:"));
          console.error(chalk.cyan("      az logout"));
          console.error(chalk.cyan("      az login --scope https://graph.microsoft.com//.default"));
        } else if (msg.includes("Insufficient privileges") || msg.includes("Authorization_RequestDenied")) {
          console.error(chalk.dim("    The signed-in identity needs Application Administrator (or higher) at tenant scope."));
        } else {
          console.error(chalk.dim("    The signed-in identity needs Directory.Read.All to query app registrations."));
        }
        process.exit(1);
      }

      if (existing) {
        kvLine("Status", chalk.green("already provisioned"));
        kvLine("App ID (client)", existing.appId);
        kvLine("Object ID", existing.id);
        kvLine("Display name", existing.displayName);
        kvLine("Identifier URI", existing.identifierUris.join(", "));

        // Make sure the SP exists too (the app reg without an SP is unusable)
        let sp = await findServicePrincipal(existing.appId);
        if (!sp) {
          if (opts.dryRun) {
            console.log(chalk.yellow("  ⚠ Service principal missing — would be created (dry-run, skipped)."));
            return;
          }
          console.log(chalk.yellow("  ⚠ App reg exists but service principal is missing — creating it now."));
          try {
            sp = await createServicePrincipal(existing.appId);
          } catch (e) {
            console.error(chalk.red(`  ✘ az ad sp create failed: ${(e as Error).message}`));
            process.exit(1);
          }
          kvLine("Service principal ID", sp.id);
        } else {
          kvLine("Service principal ID", sp.id);
        }
        printSuccess(tenant.tenantId, existing.appId);
        return;
      }

      kvLine("Status", chalk.yellow("not provisioned — will create"));
      kvLine("Display name", opts.displayName);
      kvLine("Identifier URI", AGENTMESH_IDENTIFIER_URI);
      kvLine("Sign-in audience", "AzureADMyOrg (single-tenant)");

      if (opts.dryRun) {
        console.log();
        console.log(chalk.yellow("  ⚠ --dry-run: no changes were made."));
        console.log(chalk.dim("    Re-run without --dry-run to provision."));
        return;
      }

      // Step 3: create the app + SP
      section("Provisioning");
      let app: AppRegistration;
      try {
        app = await createApp(opts.displayName);
      } catch (e) {
        const msg = (e as Error).message;
        console.error(chalk.red(`  ✘ az ad app create failed: ${msg}`));
        if (msg.includes("Insufficient privileges") || msg.includes("Authorization_RequestDenied")) {
          console.error(chalk.dim("    The signed-in identity needs Application Administrator (or higher) at tenant scope."));
        }
        process.exit(1);
      }
      kvLine("App ID (client)", app.appId);
      kvLine("Object ID", app.id);

      let sp: ServicePrincipal;
      try {
        sp = await createServicePrincipal(app.appId);
      } catch (e) {
        console.error(chalk.red(`  ✘ az ad sp create failed: ${(e as Error).message}`));
        console.error(chalk.dim("    The app reg was created but its service principal was not. Re-run this command to finish."));
        process.exit(1);
      }
      kvLine("Service principal ID", sp.id);

      printSuccess(tenant.tenantId, app.appId);
    });
}

function printSuccess(tenantId: string, appId: string): void {
  console.log();
  console.log(chalk.green("  ✓ ") + chalk.bold("AGT trust foundation is in place."));
  console.log();
  console.log(chalk.dim("    Sandboxes in this tenant will register as the AGT verified tier on next pod"));
  console.log(chalk.dim("    restart — no controller change, no Helm upgrade. The entrypoint's Workload"));
  console.log(chalk.dim(`    Identity → Entra token exchange will succeed for ${chalk.cyan("api://agentmesh/.default")}.`));
  console.log();
  console.log(chalk.dim("    Tenant ID  : ") + chalk.cyan(tenantId));
  console.log(chalk.dim("    Client ID  : ") + chalk.cyan(appId));
  console.log();
  console.log(chalk.dim("    To revert: ") + chalk.cyan(`az ad app delete --id ${appId}`));
}
