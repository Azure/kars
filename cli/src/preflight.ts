// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Preflight RBAC & provider checks for `azureclaw up`.
 *
 * `azureclaw up` takes ~15–25 minutes end-to-end. Failing halfway through
 * because the caller lacks `Microsoft.Authorization/roleAssignments/write`
 * is a terrible experience. This module queries the caller's effective
 * permissions at subscription scope, resource provider registration
 * state, and preview feature flags BEFORE Bicep runs, so we fail fast
 * with copy-pasteable remediation commands.
 *
 * Checks performed:
 *   1. az account show — caller signed in; capture tenant/sub/user
 *   2. Effective actions at subscription scope (via Microsoft.Authorization/permissions REST)
 *      diffed against the minimum set that `up` needs
 *   3. Required resource provider registration
 *   4. Preview feature flags (EncryptionAtHost always; KataVMIsolationPreview if confidential)
 *   5. Best-effort Entra warning about the `api://agentmesh` scope
 *
 * Exits non-zero on blocking failures. Pass `--skip-preflight` to bypass
 * (for environments where the caller knows their RBAC is correct but the
 * check API returns a misleading result, e.g. cross-tenant guests).
 */

import chalk from "chalk";
import ora from "ora";
import { execa } from "execa";

const BLUE = chalk.hex("#0078D4");

export interface PreflightOptions {
  region: string;
  resourceGroup: string;
  isolation: "standard" | "enhanced" | "confidential" | string;
  /** If set, we don't need Microsoft.CognitiveServices write (external Foundry) */
  foundryEndpoint?: string;
  /** If set, skip all preflight checks (escape hatch). */
  skipPreflight?: boolean;
}

export interface PreflightResult {
  ok: boolean;
  blocking: string[];
  warnings: string[];
  subscription?: string;
  tenant?: string;
  user?: string;
}

// Minimum set of Azure control-plane actions `azureclaw up` requires.
// Each entry lists the action and a short human label for error output.
interface RequiredAction {
  action: string;
  why: string;
  /** Optional predicate — only require this action when predicate returns true. */
  when?: (opts: PreflightOptions) => boolean;
}

const REQUIRED_ACTIONS: RequiredAction[] = [
  { action: "Microsoft.Resources/subscriptions/resourceGroups/write", why: "create the resource group" },
  { action: "Microsoft.Resources/deployments/write", why: "run the Bicep deployment" },
  { action: "Microsoft.ContainerService/managedClusters/write", why: "provision AKS" },
  { action: "Microsoft.ContainerService/managedClusters/listClusterUserCredential/action", why: "az aks get-credentials" },
  { action: "Microsoft.ContainerRegistry/registries/write", why: "provision ACR" },
  { action: "Microsoft.ContainerRegistry/registries/importImage/action", why: "import sandbox images into ACR" },
  { action: "Microsoft.KeyVault/vaults/write", why: "provision the Key Vault for sandbox secrets" },
  { action: "Microsoft.ManagedIdentity/userAssignedIdentities/write", why: "create Workload Identity" },
  { action: "Microsoft.ManagedIdentity/userAssignedIdentities/federatedIdentityCredentials/write", why: "federate Workload Identity to the sandbox ServiceAccount" },
  { action: "Microsoft.OperationalInsights/workspaces/write", why: "provision Log Analytics workspace" },
  { action: "Microsoft.Authorization/roleAssignments/write", why: "attach ACR to AKS and grant Workload Identity RBAC" },
  { action: "Microsoft.Network/virtualNetworks/write", why: "AKS VNet (if Bicep creates one)" },
  { action: "Microsoft.CognitiveServices/accounts/write", why: "provision Azure AI Foundry project", when: (o) => !o.foundryEndpoint },
];

// Role names that satisfy most of the above at subscription scope.
// Printed as remediation hints.
const REMEDIATION_ROLES = [
  "Contributor (Microsoft.Resources, AKS, ACR, KV, Monitor, MI)",
  "User Access Administrator (Microsoft.Authorization/roleAssignments/*)",
  // Or the single role that covers both:
  "— OR — Owner (covers both, but violates least-privilege)",
];

// Required resource providers. `azureclaw up` will attempt `az provider register`
// on Microsoft.Compute / Microsoft.ContainerService explicitly; others are
// typically auto-registered on first use but checking upfront catches locked-down
// subs that block auto-registration.
const REQUIRED_PROVIDERS = [
  "Microsoft.ContainerService",
  "Microsoft.ContainerRegistry",
  "Microsoft.KeyVault",
  "Microsoft.ManagedIdentity",
  "Microsoft.OperationalInsights",
  "Microsoft.Insights",
  "Microsoft.Network",
  "Microsoft.Compute",
  "Microsoft.Authorization",
];

/**
 * Match a permission-set wildcard (e.g. `Microsoft.ContainerService/*`) against
 * a concrete action string. `*` matches any characters including `/`.
 * Case-insensitive — Azure action matching is case-insensitive.
 */
export function matchAction(pattern: string, action: string): boolean {
  const escaped = pattern
    .replace(/[.+?^${}()|[\]\\]/g, "\\$&") // regex-escape literals
    .replace(/\*/g, ".*");
  return new RegExp("^" + escaped + "$", "i").test(action);
}

interface PermissionSet {
  actions?: string[];
  notActions?: string[];
  dataActions?: string[];
  notDataActions?: string[];
}

export function hasEffectiveAction(perms: PermissionSet[], action: string): boolean {
  for (const p of perms) {
    const granted = (p.actions || []).some((a) => matchAction(a, action));
    if (!granted) continue;
    const denied = (p.notActions || []).some((a) => matchAction(a, action));
    if (!denied) return true;
  }
  return false;
}

async function fetchSubscriptionPermissions(subscriptionId: string): Promise<PermissionSet[]> {
  // az rest auto-uses caller's AAD token and the management.azure.com audience.
  // NOTE: do NOT pass `--url-parameters ""` — Azure CLI splits each entry on
  // `=` and an empty string crashes with `ValueError: not enough values to
  // unpack`. The flag is optional; omit it when there are no extra params.
  const { stdout } = await execa(
    "az",
    [
      "rest",
      "--method", "GET",
      "--url", `/subscriptions/${subscriptionId}/providers/Microsoft.Authorization/permissions?api-version=2022-04-01`,
    ],
    { stdio: "pipe", timeout: 20000 }
  );
  const parsed = JSON.parse(stdout || "{}");
  return Array.isArray(parsed.value) ? parsed.value : [];
}

async function providerRegistrationState(providerNamespace: string): Promise<string | null> {
  try {
    const { stdout } = await execa(
      "az",
      ["provider", "show", "--namespace", providerNamespace, "--query", "registrationState", "-o", "tsv"],
      { stdio: "pipe", timeout: 15000 }
    );
    return stdout.trim() || null;
  } catch {
    return null;
  }
}

async function featureRegistrationState(
  providerNamespace: string,
  featureName: string
): Promise<string | null> {
  try {
    const { stdout } = await execa(
      "az",
      [
        "feature", "show",
        "--namespace", providerNamespace,
        "--name", featureName,
        "--query", "properties.state",
        "-o", "tsv",
      ],
      { stdio: "pipe", timeout: 15000 }
    );
    return stdout.trim() || null;
  } catch {
    return null;
  }
}

/**
 * Run all preflight checks and print results. Returns a PreflightResult.
 * The caller decides whether to exit on `!ok`.
 */
export async function runPreflightChecks(opts: PreflightOptions): Promise<PreflightResult> {
  const result: PreflightResult = { ok: true, blocking: [], warnings: [] };

  if (opts.skipPreflight) {
    console.log(chalk.yellow("\n  ⚠ Preflight skipped (--skip-preflight)\n"));
    return result;
  }

  console.log(BLUE("\n  ── Preflight: Azure permissions & prerequisites ──────────"));

  // 1. Caller signed in
  let spin = ora({ text: "Checking Azure sign-in...", color: "cyan" }).start();
  let account: { id: string; tenantId: string; user?: { name?: string } };
  try {
    const { stdout } = await execa("az", ["account", "show", "-o", "json"], {
      stdio: "pipe",
      timeout: 15000,
    });
    account = JSON.parse(stdout);
    result.subscription = account.id;
    result.tenant = account.tenantId;
    result.user = account.user?.name;
    spin.succeed(`Signed in as ${chalk.bold(account.user?.name ?? "<unknown>")} (sub: ${account.id})`);
  } catch {
    spin.fail("Not signed in to Azure — run `az login`");
    result.ok = false;
    result.blocking.push("Run `az login` and try again.");
    return result;
  }

  // 2. RBAC — effective permissions at subscription scope
  spin = ora({ text: "Evaluating RBAC at subscription scope...", color: "cyan" }).start();
  let perms: PermissionSet[] = [];
  try {
    perms = await fetchSubscriptionPermissions(account.id);
  } catch (e) {
    spin.warn(`Could not read effective permissions — ${(e as Error).message.split("\n")[0]}`);
    result.warnings.push(
      "RBAC check inconclusive. If `up` fails with an authorization error, re-run with adequate roles."
    );
  }

  if (perms.length > 0) {
    const missing: RequiredAction[] = [];
    for (const req of REQUIRED_ACTIONS) {
      if (req.when && !req.when(opts)) continue;
      if (!hasEffectiveAction(perms, req.action)) missing.push(req);
    }
    if (missing.length === 0) {
      spin.succeed(`RBAC — all ${REQUIRED_ACTIONS.filter((r) => !r.when || r.when(opts)).length} required actions granted`);
    } else {
      spin.fail(`RBAC — ${missing.length} required action(s) missing at subscription scope`);
      for (const m of missing) {
        console.log(chalk.red(`      ✗ ${m.action}`) + chalk.dim(`  — ${m.why}`));
      }
      result.ok = false;
      result.blocking.push(
        `Grant the current user sufficient RBAC. At minimum you need the roles:\n      ${REMEDIATION_ROLES.map((r) => chalk.cyan(r)).join("\n      ")}\n\n      Ask your subscription Owner / Global Admin to run:\n      ${chalk.cyan(`az role assignment create --assignee ${account.user?.name ?? "<your-user>"} --role "Contributor" --scope /subscriptions/${account.id}`)}\n      ${chalk.cyan(`az role assignment create --assignee ${account.user?.name ?? "<your-user>"} --role "User Access Administrator" --scope /subscriptions/${account.id}`)}`
      );
    }
  }

  // 3. Resource providers
  spin = ora({ text: "Checking resource provider registration...", color: "cyan" }).start();
  const providerStates: Array<{ ns: string; state: string | null }> = [];
  for (const ns of REQUIRED_PROVIDERS) {
    providerStates.push({ ns, state: await providerRegistrationState(ns) });
  }
  // Microsoft.CognitiveServices only required when provisioning a new Foundry
  if (!opts.foundryEndpoint) {
    providerStates.push({
      ns: "Microsoft.CognitiveServices",
      state: await providerRegistrationState("Microsoft.CognitiveServices"),
    });
  }

  const unregistered = providerStates.filter((p) => p.state !== "Registered");
  if (unregistered.length === 0) {
    spin.succeed(`Resource providers — ${providerStates.length} registered`);
  } else {
    const notFound = unregistered.filter((p) => p.state === null);
    const pending = unregistered.filter((p) => p.state !== null);
    if (pending.length > 0) {
      spin.warn(
        `Resource providers — ${pending.length} not yet registered (${pending.map((p) => p.ns).join(", ")})`
      );
      result.warnings.push(
        `${pending.length} resource provider(s) need registration. Run:\n      ${pending
          .map((p) => chalk.cyan(`az provider register -n ${p.ns}`))
          .join("\n      ")}`
      );
    }
    if (notFound.length > 0) {
      spin = ora().fail(
        `Resource providers — could not verify ${notFound.length} (${notFound.map((p) => p.ns).join(", ")})`
      );
      result.warnings.push(
        `Could not read registration state for: ${notFound.map((p) => p.ns).join(", ")}. Verify network access to management.azure.com.`
      );
    }
  }

  // 4. Preview feature flags
  spin = ora({ text: "Checking preview feature flags...", color: "cyan" }).start();
  const requiredFeatures: Array<{ ns: string; name: string; required: boolean }> = [
    { ns: "Microsoft.Compute", name: "EncryptionAtHost", required: true },
  ];
  if (opts.isolation === "confidential") {
    requiredFeatures.push({
      ns: "Microsoft.ContainerService",
      name: "KataVMIsolationPreview",
      required: true,
    });
  }
  const featureStates = await Promise.all(
    requiredFeatures.map(async (f) => ({
      ...f,
      state: await featureRegistrationState(f.ns, f.name),
    }))
  );
  const unregisteredFeatures = featureStates.filter((f) => f.state !== "Registered");
  if (unregisteredFeatures.length === 0) {
    spin.succeed(`Preview features — ${featureStates.length} registered`);
  } else {
    spin.warn(
      `Preview features — ${unregisteredFeatures.length} not yet registered (${unregisteredFeatures
        .map((f) => f.name)
        .join(", ")})`
    );
    // Not blocking: up.ts attempts `az feature register` automatically, but
    // feature propagation can take 10+ minutes. Warn so the operator knows.
    result.warnings.push(
      `${unregisteredFeatures.length} feature(s) need registration (may take 5–15 min to propagate):\n      ${unregisteredFeatures
        .map((f) => chalk.cyan(`az feature register --namespace ${f.ns} --name ${f.name}`))
        .join("\n      ")}`
    );
  }

  // 5. Tenant-level warning about api://agentmesh scope
  //    We can't actually verify this from the CLI without Graph permissions.
  //    Just surface the known caveat so operators aren't surprised later.
  console.log(
    chalk.dim(
      `      · AGT scope ${chalk.cyan("api://agentmesh")} — requires tenant admin to register (sandboxes fall back to AGT anonymous tier otherwise).`
    )
  );

  // Summary
  console.log();
  if (!result.ok) {
    console.log(chalk.red(`  ✗ Preflight failed — ${result.blocking.length} blocking issue(s)`));
    for (const b of result.blocking) {
      console.log(chalk.red(`    • `) + b.replace(/\n/g, "\n      "));
    }
    if (result.warnings.length > 0) {
      console.log(chalk.yellow(`\n  ${result.warnings.length} warning(s):`));
      for (const w of result.warnings) {
        console.log(chalk.yellow(`    • `) + w.replace(/\n/g, "\n      "));
      }
    }
    console.log(
      chalk.dim(
        `\n  See ${chalk.cyan("docs/permissions.md")} for the full permission matrix, or bypass with ${chalk.cyan("--skip-preflight")}.`
      )
    );
  } else if (result.warnings.length > 0) {
    console.log(chalk.yellow(`  ⚠ Preflight passed with ${result.warnings.length} warning(s)`));
    for (const w of result.warnings) {
      console.log(chalk.yellow(`    • `) + w.replace(/\n/g, "\n      "));
    }
  } else {
    console.log(chalk.green(`  ✓ Preflight passed — permissions & prerequisites OK`));
  }
  console.log();

  return result;
}
