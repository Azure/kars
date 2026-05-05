// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// `azureclaw mesh setup-trust` — provisions the Entra ID app registration
// and service principal that AzureClaw sandboxes use to acquire access
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
      `Provision the tenant-wide Entra app registration (${AGENTMESH_IDENTIFIER_URI}) so sandboxes register as the AGT verified tier`,
    )
    .option("--display-name <name>", "Display name for the Entra app registration", "AzureClaw AgentMesh")
    .option("--dry-run", "Print what would be created without making changes", false)
    .action(async (opts: { displayName: string; dryRun: boolean }) => {
      banner("AzureClaw · Mesh Setup Trust", "Entra App Registration for api://agentmesh");

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
