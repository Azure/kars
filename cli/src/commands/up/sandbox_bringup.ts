// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Sub-slice S15.d.4 of S15.d phase2-hotspot-up-cli.
//
// Sandbox bring-up phase extracted verbatim from cli/src/commands/up.ts:
//   - federated credential creation (sandbox + controller SA)
//   - MI Contributor self-scoped role assignment
//   - Foundry RBAC via Bicep (sandbox WI, project MI, kubelet MI)
//   - ClawSandbox CR creation
//   - wait for sandbox Running + WebUI port-forward
//   - deployment summary + saveContext()
import path from "node:path";
import { writeFileSync, unlinkSync } from "node:fs";
import chalk from "chalk";
import type { Stepper } from "../../stepper.js";
import { section, kvLine, checkLine } from "../../stepper.js";
import { saveContext } from "../../config.js";
import {
  buildInferencePolicy,
  buildToolPolicy,
  inferenceRefName,
  toolPolicyRefName,
} from "../../refs.js";

export interface SandboxBringUpContext {
  options: {
    name: string;
    model: string;
    region: string;
    isolation: string;
    [key: string]: unknown;
  };
  baseName: string;
  rg: string;
  acrLoginServer: string;
  foundryEndpoint: string;
  openAiEndpoint: string;
  kvName: string;
  wiClientId: string;
  imdsClientId: string;
  repoRoot: string;
  stepper: Stepper;
  registryMode: "local" | "global";
  globalRegistryUrl?: string;
  globalRelayUrl?: string;
}

/**
 * Execute Step 7 (federated credentials + RBAC + ClawSandbox CR), Step 8
 * (wait for Running + WebUI port-forward), and the deployment summary +
 * saveContext() at end-of-deploy.
 *
 * Body moved verbatim from up.ts; identical side effects and exit codes.
 */
export async function bringUpSandbox(ctx: SandboxBringUpContext): Promise<void> {
  const { execa } = await import("execa");
  const {
    options, baseName, rg,
    acrLoginServer, foundryEndpoint, openAiEndpoint, kvName,
    wiClientId, imdsClientId, repoRoot, stepper,
    registryMode, globalRegistryUrl, globalRelayUrl,
  } = ctx;

  // ── Step 7: Create ClawSandbox CR ────────────────────────────
  stepper.step(`Creating sandbox '${options.name}'...`);
  const sandboxNs = `azureclaw-${options.name}`;

  // Create federated identity credential for this sandbox's namespace
  stepper.update(`Setting up Workload Identity for ${sandboxNs}...`);
  const { stdout: oidcIssuer } = await execa("az", [
    "aks", "show",
    "--name", `${baseName}-aks`,
    "--resource-group", rg,
    "--query", "oidcIssuerProfile.issuerUrl",
    "--output", "tsv",
  ], { stdio: "pipe" });

  await execa("az", [
    "identity", "federated-credential", "create",
    "--identity-name", `${baseName}-aks-sandbox-wi`,
    "--resource-group", rg,
    "--name", `azureclaw-${options.name}`,
    "--issuer", oidcIssuer.trim(),
    "--subject", `system:serviceaccount:${sandboxNs}:sandbox`,
    "--audiences", "api://AzureADTokenExchange",
    "--output", "none",
  ], { stdio: "pipe" }).then(() => {
    stepper.detail("new", `Federated credential — ${sandboxNs}:sandbox`);
  }).catch(() => {
    stepper.detail("ok", `Federated credential — already exists`);
  });

  // Ensure controller SA has a fedcred too (so it can get ARM tokens via WI to create sandbox fedcreds)
  await execa("az", [
    "identity", "federated-credential", "create",
    "--identity-name", `${baseName}-aks-sandbox-wi`,
    "--resource-group", rg,
    "--name", `azureclaw-controller-sa`,
    "--issuer", oidcIssuer.trim(),
    "--subject", `system:serviceaccount:azureclaw-system:azureclaw-controller`,
    "--audiences", "api://AzureADTokenExchange",
    "--output", "none",
  ], { stdio: "pipe" }).then(() => {
    stepper.detail("new", `Federated credential — controller SA`);
  }).catch(() => {
    // Already exists — fine
  });

  // Grant the sandbox MI "Managed Identity Contributor" on itself so the controller
  // can create/delete fedcreds for dynamically spawned sandboxes
  try {
    const { stdout: subIdForMi } = await execa("az", [
      "account", "show", "--query", "id", "--output", "tsv",
    ], { stdio: "pipe", timeout: 10000 });
    const miScope = `/subscriptions/${subIdForMi.trim()}/resourceGroups/${rg}/providers/Microsoft.ManagedIdentity/userAssignedIdentities/${baseName}-aks-sandbox-wi`;
    const { stdout: miPid } = await execa("az", [
      "identity", "show",
      "--name", `${baseName}-aks-sandbox-wi`,
      "--resource-group", rg,
      "--query", "principalId",
      "--output", "tsv",
    ], { stdio: "pipe" });
    await execa("az", [
      "role", "assignment", "create",
      "--assignee-object-id", miPid.trim(),
      "--assignee-principal-type", "ServicePrincipal",
      "--role", "Managed Identity Contributor",
      "--scope", miScope,
      "--output", "none",
    ], { stdio: "pipe" });
    stepper.detail("new", `MI Contributor — self-scoped for fedcred management`);
  } catch {
    // Already exists or user lacks Owner — non-fatal
  }

  // Grant RBAC roles on Foundry resource via Bicep (if --foundry-endpoint provided)
  // Two assignments needed:
  //   1. Sandbox WI → Azure AI User on the Foundry AI Services resource (so pods can call APIs)
  //   2. Foundry project MI → Azure AI User on the resource group (so Memory Store can call models internally)
  if (foundryEndpoint) {
    stepper.update("Configuring Foundry project RBAC (via Bicep)...");
    const foundryHost = new URL(foundryEndpoint).hostname;
    // Extract account name: "foo.services.ai.azure.com" → "foo", or "foo.openai.azure.com" → "foo"
    const foundryAccountName = foundryHost.split(".")[0];

    // Extract project name from URL path: "/api/projects/bar" → "bar"
    const foundryUrl = new URL(foundryEndpoint);
    const projectMatch = foundryUrl.pathname.match(/\/api\/projects\/([^/]+)/);
    const foundryProjectName = projectMatch ? projectMatch[1] : "";

    // Find the Foundry AI Services account and its resource group
    const { stdout: foundryAccountJson } = await execa("az", [
      "cognitiveservices", "account", "list",
      "--query", `[?name=='${foundryAccountName}'].{id:id, rg:resourceGroup} | [0]`,
      "--output", "json",
    ], { stdio: "pipe" }).catch(() => ({ stdout: "{}" }));

    const foundryAccount = JSON.parse(foundryAccountJson.trim() || "{}");
    const foundryResourceId = foundryAccount.id || "";
    const foundryRg = foundryAccount.rg || "";

    if (foundryResourceId && foundryRg && foundryProjectName) {
      // Query the project's managed identity principal ID via ARM REST API
      let projectMiPrincipalId = "";
      try {
        const { stdout: projectJson } = await execa("az", [
          "rest", "--method", "get",
          "--url", `${foundryResourceId}/projects/${foundryProjectName}?api-version=2025-06-01`,
        ], { stdio: "pipe" });
        const project = JSON.parse(projectJson.trim());
        projectMiPrincipalId = project?.identity?.principalId || "";
      } catch {
        // Project may not have system MI enabled — warn but continue
      }

      // Get the sandbox workload identity principal ID
      let sandboxWiPrincipalId = "";
      try {
        const { stdout: wiPid } = await execa("az", [
          "identity", "show",
          "--name", `${baseName}-aks-sandbox-wi`,
          "--resource-group", rg,
          "--query", "principalId",
          "--output", "tsv",
        ], { stdio: "pipe" });
        sandboxWiPrincipalId = wiPid.trim().split("\n").pop()?.trim() || "";
      } catch {
        // Non-fatal
      }

      // Get the AKS kubelet managed identity principal ID (used by IMDS for sub-agents)
      let kubeletMiPrincipalId = "";
      try {
        const { stdout: kubePid } = await execa("az", [
          "aks", "show",
          "--name", `${baseName}-aks`,
          "--resource-group", rg,
          "--query", "identityProfile.kubeletidentity.objectId",
          "--output", "tsv",
        ], { stdio: "pipe" });
        kubeletMiPrincipalId = kubePid.trim().split("\n").pop()?.trim() || "";
      } catch {
        // Non-fatal — older AKS may not expose this
      }

      // Build Bicep that assigns roles via deployment (bypasses CLI conditional access)
      const bicepLines = [
        "targetScope = 'resourceGroup'",
        "param sandboxWiPrincipalId string",
        "param projectMiPrincipalId string",
        "param kubeletMiPrincipalId string",
        `param foundryAccountName string = '${foundryAccountName}'`,
        "",
        "// Azure AI User role ID — has Microsoft.CognitiveServices/* wildcard data actions",
        "var azureAiUser = '53ca6127-db72-4b80-b1b0-d745d6d5456d'",
        "// Cognitive Services OpenAI User — explicit data-plane access for chat completions",
        "var cogSvcOpenAiUser = '5e0bd9bd-7b93-4f28-af87-19fc36ad61bd'",
        "",
        "resource aiServices 'Microsoft.CognitiveServices/accounts@2024-10-01' existing = {",
        "  name: foundryAccountName",
        "}",
        "",
        "// 1. Sandbox WI → Azure AI User on the AI Services resource (pod API access)",
        "resource sandboxRole 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (!empty(sandboxWiPrincipalId)) {",
        "  name: guid(aiServices.id, sandboxWiPrincipalId, 'azure-ai-user')",
        "  scope: aiServices",
        "  properties: {",
        "    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', azureAiUser)",
        "    principalId: sandboxWiPrincipalId",
        "    principalType: 'ServicePrincipal'",
        "  }",
        "}",
        "",
        "// 1b. Sandbox WI → Cognitive Services OpenAI User (explicit chat completions data action)",
        "resource sandboxOpenAiRole 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (!empty(sandboxWiPrincipalId)) {",
        "  name: guid(aiServices.id, sandboxWiPrincipalId, 'cog-svc-openai-user')",
        "  scope: aiServices",
        "  properties: {",
        "    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', cogSvcOpenAiUser)",
        "    principalId: sandboxWiPrincipalId",
        "    principalType: 'ServicePrincipal'",
        "  }",
        "}",
        "",
        "// 2. Project MI → Azure AI User on the resource group (Memory Store internal model calls)",
        "resource projectMiRole 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (!empty(projectMiPrincipalId)) {",
        "  name: guid(resourceGroup().id, projectMiPrincipalId, 'azure-ai-user')",
        "  properties: {",
        "    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', azureAiUser)",
        "    principalId: projectMiPrincipalId",
        "    principalType: 'ServicePrincipal'",
        "  }",
        "}",
        "",
        "// 3. Kubelet MI → Cognitive Services OpenAI User (IMDS fallback for spawned sub-agents)",
        "resource kubeletOpenAiRole 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (!empty(kubeletMiPrincipalId)) {",
        "  name: guid(aiServices.id, kubeletMiPrincipalId, 'cog-svc-openai-user')",
        "  scope: aiServices",
        "  properties: {",
        "    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', cogSvcOpenAiUser)",
        "    principalId: kubeletMiPrincipalId",
        "    principalType: 'ServicePrincipal'",
        "  }",
        "}",
      ];

      const tmpBicep = path.join(repoRoot, ".tmp-foundry-rbac.bicep");
      writeFileSync(tmpBicep, bicepLines.join("\n"));

      try {
        stepper.update("Deploying Foundry RBAC (Bicep)...");
        await execa("az", [
          "deployment", "group", "create",
          "--resource-group", foundryRg,
          "--template-file", tmpBicep,
          "--parameters",
          `sandboxWiPrincipalId=${sandboxWiPrincipalId}`,
          `projectMiPrincipalId=${projectMiPrincipalId}`,
          `kubeletMiPrincipalId=${kubeletMiPrincipalId}`,
          "--output", "none",
        ], { stdio: "pipe" });
      } catch {
        // Non-fatal — user may lack Owner on the Foundry RG
      } finally {
        try { unlinkSync(tmpBicep); } catch {}
      }

      if (!projectMiPrincipalId) {
        console.log(chalk.yellow("\n  ⚠ Foundry project has no system-assigned MI. Memory Store will not work."));
        console.log(chalk.yellow("    Enable it: Portal → Project → Resource Management → Identity → System assigned → On"));
        console.log(chalk.yellow("    Then re-run: azureclaw up ...\n"));
      }
    } else if (foundryResourceId) {
      // Fallback for non-project endpoints (plain AOAI): assign sandbox WI on the resource
      const { stdout: wiPid } = await execa("az", [
        "identity", "show",
        "--name", `${baseName}-aks-sandbox-wi`,
        "--resource-group", rg,
        "--query", "principalId",
        "--output", "tsv",
      ], { stdio: "pipe" }).catch(() => ({ stdout: "" }));

      if (wiPid.trim()) {
        const tmpBicep = path.join(repoRoot, ".tmp-foundry-rbac.bicep");
        writeFileSync(tmpBicep, [
          "targetScope = 'resourceGroup'",
          "param pid string",
          `param accountName string = '${foundryAccountName}'`,
          "resource acct 'Microsoft.CognitiveServices/accounts@2024-10-01' existing = { name: accountName }",
          "resource r 'Microsoft.Authorization/roleAssignments@2022-04-01' = {",
          "  name: guid(acct.id, pid, 'azure-ai-user')",
          "  scope: acct",
          "  properties: {",
          "    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '53ca6127-db72-4b80-b1b0-d745d6d5456d')",
          "    principalId: pid",
          "    principalType: 'ServicePrincipal'",
          "  }",
          "}",
        ].join("\n"));
        try {
          await execa("az", [
            "deployment", "group", "create",
            "--resource-group", foundryRg || rg,
            "--template-file", tmpBicep,
            "--parameters", `pid=${wiPid.trim().split("\n").pop()?.trim()}`,
            "--output", "none",
          ], { stdio: "pipe" }).catch(() => {});
        } finally {
          try { unlinkSync(tmpBicep); } catch {}
        }
      }
    }
  }

  stepper.update(`Creating sandbox '${options.name}'...`);
  const sandboxNamespace = "azureclaw-system";
  const inferencePolicy = buildInferencePolicy({
    sandboxName: options.name,
    namespace: sandboxNamespace,
    model: options.model,
    provider: "azure-openai",
    contentSafety: true,
    promptShields: true,
  });
  const toolPolicy = buildToolPolicy({
    sandboxName: options.name,
    namespace: sandboxNamespace,
    profile: "default",
  });
  const sandboxManifest = {
    apiVersion: "azureclaw.azure.com/v1alpha1",
    kind: "ClawSandbox",
    metadata: {
      name: options.name,
      namespace: sandboxNamespace,
    },
    spec: {
      runtime: {
        kind: "OpenClaw",
        openclaw: {
          image: `${acrLoginServer}/openclaw-sandbox:latest`,
        },
      },
      sandbox: {
        isolation: options.isolation,
      },
      inferenceRef: {
        name: inferenceRefName(options.name),
      },
      networkPolicy: {
        defaultDeny: true,
        approvalRequired: true,
        learnEgress: true,
      },
      governance: {
        enabled: true,
        toolPolicyRef: { name: toolPolicyRefName(options.name) },
        trustThreshold: 500,
      },
    },
  };
  const bundleManifest = {
    apiVersion: "v1",
    kind: "List",
    items: [inferencePolicy, toolPolicy, sandboxManifest],
  };
  await execa("kubectl", ["apply", "-f", "-"], {
    input: JSON.stringify(bundleManifest),
    stdio: ["pipe", "pipe", "pipe"],
  });

  // ── Step 8: Wait for sandbox ─────────────────────────────────
  stepper.step("Waiting for sandbox to start...");
  await execa("kubectl", [
    "wait",
    "--for=jsonpath={.status.phase}=Running",
    `clawsandbox/${options.name}`,
    "-n", "azureclaw-system",
    "--timeout=120s",
  ], { stdio: "pipe" }).catch(() => {
    // Timeout OK — image pull may be slow on first deploy
  });

  // Extract gateway token and start port-forward
  stepper.update("Setting up WebUI access...");
  let gatewayToken = "";
  let webUiUrl = "";
  try {
    // Wait for gateway to be ready inside the pod
    await new Promise(r => setTimeout(r, 5000));

    // Extract gateway token from the sandbox
    const { stdout: bashrc } = await execa("kubectl", [
      "exec", "-n", sandboxNs, `deploy/${options.name}`,
      "-c", "openclaw", "--",
      "cat", "/sandbox/.bashrc",
    ], { stdio: "pipe" });
    const tokenMatch = bashrc.match(/OPENCLAW_GATEWAY_TOKEN="([^"]+)"/);
    if (tokenMatch) {
      gatewayToken = tokenMatch[1];
    }

    // Start port-forward in background (fully detached so CLI can exit)
    const { spawn } = await import("child_process");
    const portForward = spawn("kubectl", [
      "port-forward", "-n", sandboxNs,
      `deploy/${options.name}`, "18789:18789",
    ], { stdio: "ignore", detached: true });
    portForward.unref();
    // Give it a moment to bind
    await new Promise(r => setTimeout(r, 2000));

    if (gatewayToken) {
      webUiUrl = `http://localhost:18789/#token=${gatewayToken}`;
    }

    stepper.done("Sandbox running");
  } catch {
    stepper.warn("Sandbox running but WebUI port-forward failed");
  }

  stepper.summary();

  // ── Summary ──────────────────────────────────────────────────
  const isolationDesc: Record<string, string> = {
    standard: "standard (runc + RuntimeDefault)",
    enhanced: "enhanced (runc + azureclaw-strict seccomp)",
    confidential: "confidential (Kata VM isolation)",
  };

  section("Deployment");
  kvLine("Sandbox", options.name);
  kvLine("Model", `${options.model} (Azure OpenAI, Entra ID auth)`);
  kvLine("Isolation", isolationDesc[options.isolation] || options.isolation);
  kvLine("Region", options.region);
  kvLine("Cluster", `${baseName}-aks`);
  kvLine("ACR", acrLoginServer);
  kvLine("Key Vault", kvName);
  kvLine("AOAI", openAiEndpoint);

  section("Security");
  checkLine(true, "Cilium CNI + NetworkPolicy (default-deny egress)");
  checkLine(true, "Workload Identity (Entra ID, no API keys)");
  checkLine(true, "Read-only rootfs, non-root, seccomp");
  checkLine(true, "Inference router: Content Safety + Prompt Shields");
  checkLine(true, "Egress proxy with domain allowlist + blocklist (51k+)");
  if (options.isolation === "confidential") {
    checkLine(true, "Kata VM isolation (pod sandboxing)");
  }

  section("Commands");
  console.log(`  Connect:     ${chalk.cyan(`azureclaw connect ${options.name}`)}`);
  console.log(`  Status:      ${chalk.cyan(`azureclaw status ${options.name}`)}`);
  console.log(`  Logs:        ${chalk.cyan(`azureclaw logs ${options.name} -f`)}`);
  console.log(`  Egress:      ${chalk.cyan(`azureclaw egress ${options.name}`)}`);

  if (webUiUrl) {
    section("WebUI");
    console.log(`  ${chalk.green("→")} ${chalk.cyan.underline(webUiUrl)}`);
  }

  // Cache deployment context for subsequent commands (add, status, list, push,
  // etc.). Setting phase: "complete" also marks the auto-resume state as fully
  // consumed so the next `azureclaw up` starts fresh.
  try {
    saveContext({
      region: options.region,
      resourceGroup: rg,
      aksCluster: `${baseName}-aks`,
      acrLoginServer,
      acrName: acrLoginServer.replace(".azurecr.io", ""),
      keyVaultName: kvName,
      wiClientId,
      imdsClientId: imdsClientId || undefined,
      foundryEndpoint: openAiEndpoint,
      foundryProjectEndpoint: foundryEndpoint || undefined,
      identityName: `${baseName}-aks-sandbox-wi`,
      identityResourceGroup: rg,
      oidcIssuerUrl: oidcIssuer?.trim() || undefined,
      registryMode,
      globalRegistryUrl,
      globalRelayUrl,
      phase: "complete",
      sandboxName: options.name,
      sourceAcr: typeof options.sourceAcr === "string" ? options.sourceAcr : undefined,
    });
  } catch { /* non-critical */ }

  console.log();
}
