// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Sub-slice S15.d.4 of S15.d phase2-hotspot-up-cli.
//
// Sandbox bring-up phase extracted verbatim from cli/src/commands/up.ts:
//   - federated credential creation (sandbox + controller SA)
//   - MI Contributor self-scoped role assignment
//   - Foundry RBAC via Bicep (sandbox WI, project MI, kubelet MI)
//   - KarsSandbox CR creation
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
  buildKarsMemory,
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
 * Execute Step 7 (federated credentials + RBAC + KarsSandbox CR), Step 8
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

  // ── Step 7: Create KarsSandbox CR ────────────────────────────
  stepper.step(`Creating sandbox '${options.name}'...`);
  const sandboxNs = `kars-${options.name}`;

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
    "--name", `kars-${options.name}`,
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
    "--name", `kars-controller-sa`,
    "--issuer", oidcIssuer.trim(),
    "--subject", `system:serviceaccount:kars-system:kars-controller`,
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
    stepper.update("Configuring Foundry project (discovery + setup + RBAC)...");

    // Discover + best-effort provision the BYO Foundry project: pick the best
    // deployed chat model, ensure an embedding model, and enable the project's
    // system-assigned MI (Memory Store authenticates internally as the project
    // MI). All idempotent + non-fatal — see foundry_setup.ts.
    const { setupFoundryForKars } = await import("./foundry_setup.js");
    const foundrySetup = await setupFoundryForKars({
      execa, stepper, foundryEndpoint,
    }).catch(() => null);

    // Adopt the best deployed chat model unless the user explicitly set --model.
    const modelExplicit = process.argv.includes("--model");
    if (foundrySetup?.bestChatModel && !modelExplicit) {
      if (foundrySetup.bestChatModel !== options.model) {
        stepper.detail("info", `Using best deployed model '${foundrySetup.bestChatModel}' (was default '${options.model}'; pass --model to override)`);
      }
      options.model = foundrySetup.bestChatModel;
    }
    for (const note of foundrySetup?.notes ?? []) {
      stepper.detail("info", note);
    }

    const foundryHost = new URL(foundryEndpoint).hostname;
    // Extract account name: "foo.services.ai.azure.com" → "foo", or "foo.openai.azure.com" → "foo"
    const foundryAccountName = foundrySetup?.accountName || foundryHost.split(".")[0];

    // Extract project name from URL path: "/api/projects/bar" → "bar"
    const foundryUrl = new URL(foundryEndpoint);
    const projectMatch = foundryUrl.pathname.match(/\/api\/projects\/([^/]+)/);
    const foundryProjectName = foundrySetup?.projectName || (projectMatch ? projectMatch[1] : "");

    // Account ARM id + resource group — reuse the discovery result, else resolve.
    let foundryResourceId = foundrySetup?.accountResourceId || "";
    let foundryRg = foundrySetup?.resourceGroup || "";
    if (!foundryResourceId || !foundryRg) {
      const { stdout: foundryAccountJson } = await execa("az", [
        "cognitiveservices", "account", "list",
        "--query", `[?name=='${foundryAccountName}'].{id:id, rg:resourceGroup} | [0]`,
        "--output", "json",
      ], { stdio: "pipe" }).catch(() => ({ stdout: "{}" }));
      const foundryAccount = JSON.parse(foundryAccountJson.trim() || "{}");
      foundryResourceId = foundryAccount.id || "";
      foundryRg = foundryAccount.rg || "";
    }

    if (foundryResourceId && foundryRg && foundryProjectName) {
      // Project MI principalId — resolved (and, if it was off, enabled) by the
      // discovery step above.
      const projectMiPrincipalId = foundrySetup?.projectMiPrincipalId || "";


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

      // Phase 5 — blueprint-SP-scoped Foundry RBAC.
      //
      // When the cluster runs in Entra Agent ID mode (Pattern A or B),
      // every per-sandbox agent identity derives from the blueprint
      // and INHERITS its role assignments. Granting the role at
      // blueprint-SP scope means:
      //   - One assignment covers every present and future sandbox.
      //   - No per-sandbox role-assignment churn as sandboxes spawn /
      //     terminate.
      // See research/critique.md R5 + docs/architecture/entra-agent-id/05-security-alignment.md.
      //
      // Source of truth: KarsAuthConfig.spec.agentId.blueprintSpObjectId
      // (Optional; populated by the modernised setup-trust flow).
      // When absent — typical on clusters bootstrapped before this
      // change — we surface a structured WARN with the exact
      // remediation command so the operator can run it once.
      let blueprintSpPrincipalId = "";
      try {
        const { stdout: kacJson } = await execa("kubectl", [
          "get", "karsauthconfig", "default", "-o", "json",
        ], { stdio: "pipe" });
        const kac = JSON.parse(kacJson);
        blueprintSpPrincipalId =
          kac?.spec?.agentId?.blueprintSpObjectId?.trim() || "";
      } catch {
        // No KAC, or kubectl unavailable — sandbox is anonymous-tier.
      }

      // Build Bicep that assigns roles via deployment (bypasses CLI conditional access)
      const bicepLines = [
        "targetScope = 'resourceGroup'",
        "param sandboxWiPrincipalId string",
        "param projectMiPrincipalId string",
        "param kubeletMiPrincipalId string",
        "param blueprintSpPrincipalId string",
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
        "",
        "// 4. Blueprint SP — informational role assignment.",
        "//    Note: Azure RBAC does NOT inherit from blueprint to derived",
        "//    agent identities. Per Microsoft docs, only Graph permissions",
        "//    are inheritable. Per-agent-identity Foundry RBAC is granted",
        "//    by the kars controller after provisioning each identity",
        "//    (see controller/src/agent_id_provisioning.rs).",
        "//    We still grant the role here so a missing controller-managed",
        "//    grant doesn't fully block the cluster — operator can fall back",
        "//    to AGT-anonymous mode via the blueprint SP itself.",
        "resource blueprintOpenAiRole 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (!empty(blueprintSpPrincipalId)) {",
        "  name: guid(aiServices.id, blueprintSpPrincipalId, 'cog-svc-openai-user')",
        "  scope: aiServices",
        "  properties: {",
        "    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', cogSvcOpenAiUser)",
        "    principalId: blueprintSpPrincipalId",
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
          `blueprintSpPrincipalId=${blueprintSpPrincipalId}`,
          "--output", "none",
        ], { stdio: "pipe" });

        if (blueprintSpPrincipalId) {
          console.log(
            chalk.dim(
              `  ✓ Blueprint SP (${blueprintSpPrincipalId.slice(0, 8)}…) granted Cognitive Services OpenAI User + Azure AI User on Foundry`,
            ),
          );
          console.log(
            chalk.dim(
              `    All present and future per-sandbox agent identities inherit these roles.`,
            ),
          );
        }
      } catch (err) {
        // The most common failure is AuthorizationFailed because the
        // signed-in user lacks `Microsoft.Authorization/roleAssignments/write`
        // on the Foundry resource. That's an organisational permissions
        // issue, not a bug — surface a structured remediation hint so
        // the operator can hand it to the Foundry owner.
        const msg = (err as { stderr?: string; message?: string }).stderr ??
          (err as Error).message ?? "";
        if (msg.includes("AuthorizationFailed") || msg.includes("roleAssignments/write")) {
          console.log();
          console.log(
            chalk.yellow(
              "  ⚠ Foundry RBAC deployment failed — your principal lacks roleAssignments/write on the Foundry resource.",
            ),
          );
          console.log(
            chalk.yellow(
              "    The kars sandbox will boot, but inference will return 401 PermissionDenied until the role is granted.",
            ),
          );
          if (blueprintSpPrincipalId) {
            console.log();
            console.log(chalk.dim("    Hand this to the Foundry resource owner (or run with Owner / User Access Administrator):"));
            console.log(chalk.dim(""));
            console.log(chalk.dim(
              `      az role assignment create \\\n` +
              `        --assignee-object-id ${blueprintSpPrincipalId} \\\n` +
              `        --assignee-principal-type ServicePrincipal \\\n` +
              `        --role "Cognitive Services OpenAI User" \\\n` +
              `        --scope "${foundryResourceId}"`
            ));
            console.log(chalk.dim(""));
            console.log(chalk.dim("    All present and future per-sandbox agent identities will inherit this role."));
          }
          console.log();
        }
        // Non-fatal — caller may also be testing a partial install
      } finally {
        try { unlinkSync(tmpBicep); } catch {}
      }

      if (!projectMiPrincipalId && !foundrySetup) {
        console.log(chalk.yellow("\n  ⚠ Foundry project has no system-assigned MI. Memory Store will not work."));
        console.log(chalk.yellow("    Enable it: Portal → Project → Resource Management → Identity → System assigned → On"));
        console.log(chalk.yellow("    Then re-run: kars up ...\n"));
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

      // ── Bing / web_search self-heal: remove DANGLING Grounding-with-Bing
      // connections ────────────────────────────────────────────────────────
      // kars uses the KEYLESS, Microsoft-managed `web_search` tool (Entra/IMDS
      // auth, no API key). But if the project has a `GroundingWithBingSearch`
      // connection — especially an `isDefault` one — pointing at a Bing
      // resource that no longer exists (e.g. it lived in a since-deleted
      // `kars-<region>` RG), Foundry routes web_search through that dead
      // connection and returns a misleading 401 "audience is incorrect
      // (https://bing.azure.com)". We detect connections whose backing
      // `metadata.ResourceId` no longer resolves and remove them so the
      // managed keyless path works. Purely advisory — never aborts the deploy,
      // and never touches a connection whose Bing resource is still alive.
      await removeDanglingBingConnections({
        execa,
        stepper,
        foundryResourceId,
        foundryProjectName,
      }).catch(() => { /* advisory only — never block the deploy */ });
    }
  }

  stepper.update(`Creating sandbox '${options.name}'...`);
  const sandboxNamespace = "kars-system";
  const inferencePolicy = buildInferencePolicy({
    sandboxName: options.name,
    namespace: sandboxNamespace,
    model: options.model,
    provider: "azure-openai",
    contentSafety: true,
    // Default off — only fail-closed on missing prompt_filter_results when
    // the operator opts in via --require-prompt-shields (their Foundry/AOAI
    // deployment must have a Content Filter that emits the annotations).
    promptShields: (options as { requirePromptShields?: boolean }).requirePromptShields === true,
  });
  const toolPolicy = buildToolPolicy({
    sandboxName: options.name,
    namespace: sandboxNamespace,
    profile: "default",
  });
  const sandboxManifest = {
    apiVersion: "kars.azure.com/v1alpha1",
    kind: "KarsSandbox",
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
        egressMode: "Learn",
      },
      governance: {
        enabled: true,
        toolPolicyRef: { name: toolPolicyRefName(options.name) },
        trustThreshold: 500,
      },
    },
  };
  // KarsMemory binding — only meaningful with a Foundry project endpoint
  // (Memory Store is a Foundry feature). Gives the sandbox the same
  // controller-managed binding `kars dev` creates, instead of relying purely
  // on the runtime's lazy store creation.
  const memoryCr = foundryEndpoint
    ? buildKarsMemory({ sandboxName: options.name, namespace: sandboxNamespace })
    : null;

  const bundleManifest = {
    apiVersion: "v1",
    kind: "List",
    items: [inferencePolicy, toolPolicy, ...(memoryCr ? [memoryCr] : []), sandboxManifest],
  };
  await execa("kubectl", ["apply", "-f", "-"], {
    input: JSON.stringify(bundleManifest),
    stdio: ["pipe", "pipe", "pipe"],
  });

  // ── CRD status report — confirm each resource applied + its phase ──
  stepper.detail("ok", "Applied CRDs:");
  const crdChecks: Array<{ kind: string; name: string; phasePath: string }> = [
    { kind: "inferencepolicy", name: inferenceRefName(options.name), phasePath: "{.status.phase}" },
    { kind: "toolpolicy", name: toolPolicyRefName(options.name), phasePath: "{.status.phase}" },
    ...(memoryCr ? [{ kind: "karsmemory", name: (memoryCr.metadata as { name: string }).name, phasePath: "{.status.phase}" }] : []),
    { kind: "karssandbox", name: options.name, phasePath: "{.status.phase}" },
  ];
  for (const c of crdChecks) {
    const { stdout: phase } = await execa("kubectl", [
      "get", c.kind, c.name, "-n", sandboxNamespace,
      "-o", `jsonpath=${c.phasePath}`,
    ], { stdio: "pipe" }).catch(() => ({ stdout: "" }));
    const ph = phase.trim();
    stepper.detail(ph && ph !== "Failed" ? "ok" : "info", `  ${c.kind}/${c.name}${ph ? ` — ${ph}` : " — applied"}`);
  }

  // ── Step 8: Wait for sandbox ─────────────────────────────────
  stepper.step("Waiting for sandbox to start...");
  await execa("kubectl", [
    "wait",
    "--for=jsonpath={.status.phase}=Running",
    `karssandbox/${options.name}`,
    "-n", "kars-system",
    "--timeout=120s",
  ], { stdio: "pipe" }).catch(() => {
    // Timeout OK — image pull may be slow on first deploy
  });

  // Extract gateway token and start port-forward
  stepper.update("Setting up WebUI access...");
  let gatewayToken = "";
  let webUiUrl = "";
  try {
    // Extract the gateway token from the sandbox. The gateway is started in
    // the background by entrypoint.sh, so on a fresh deploy `.bashrc` may not
    // be written yet — retry with backoff instead of failing the whole WebUI
    // step on a first-run race (the symptom behind "port-forward failed" even
    // though `kars connect` worked moments later).
    for (let attempt = 0; attempt < 6 && !gatewayToken; attempt++) {
      await new Promise(r => setTimeout(r, attempt === 0 ? 5000 : 3000));
      const { stdout: bashrc } = await execa("kubectl", [
        "exec", "-n", sandboxNs, `deploy/${options.name}`,
        "-c", "openclaw", "--",
        "cat", "/sandbox/.bashrc",
      ], { stdio: "pipe" }).catch(() => ({ stdout: "" }));
      const tokenMatch = bashrc.match(/OPENCLAW_GATEWAY_TOKEN="([^"]+)"/);
      if (tokenMatch) gatewayToken = tokenMatch[1];
    }

    // Pick a free local port starting at 18789. Binding 18789 unconditionally
    // is the original footgun — a stale forward, another sandbox, or `kars dev`
    // commonly holds it, so the forward fails. Mirror `kars connect` and bump
    // to the next free port instead.
    const net = await import("node:net");
    const canBind = (p: number) => new Promise<boolean>((resolve) => {
      const srv = net.createServer();
      srv.once("error", () => resolve(false));
      srv.once("listening", () => srv.close(() => resolve(true)));
      srv.listen(p, "127.0.0.1");
    });
    let localPort = 18789;
    for (let p = 18789; p < 18789 + 20; p++) {
      if (await canBind(p)) { localPort = p; break; }
    }
    if (localPort !== 18789) {
      stepper.detail("info", `Port 18789 in use — using ${localPort} for the WebUI forward`);
    }

    // Start port-forward in background (fully detached so CLI can exit)
    const { spawn } = await import("child_process");
    const portForward = spawn("kubectl", [
      "port-forward", "-n", sandboxNs,
      `deploy/${options.name}`, `${localPort}:18789`,
    ], { stdio: "ignore", detached: true });
    portForward.unref();
    // Give it a moment to bind
    await new Promise(r => setTimeout(r, 2000));

    if (gatewayToken) {
      webUiUrl = `http://localhost:${localPort}/#token=${gatewayToken}`;
      stepper.done("Sandbox running");
    } else {
      // The sandbox is up; only the token read raced. Don't claim failure —
      // `kars connect` re-derives the token and works.
      stepper.warn(`Sandbox running — WebUI token not ready yet; run 'kars connect ${options.name}'`);
    }
  } catch {
    stepper.warn(`Sandbox running but WebUI setup failed — run 'kars connect ${options.name}'`);
  }

  stepper.summary();

  // ── Summary ──────────────────────────────────────────────────
  const isolationDesc: Record<string, string> = {
    standard: "standard (runc + RuntimeDefault)",
    enhanced: "enhanced (runc + kars-strict seccomp)",
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
  console.log(`  Connect:     ${chalk.cyan(`kars connect ${options.name}`)}`);
  console.log(`  Status:      ${chalk.cyan(`kars status ${options.name}`)}`);
  console.log(`  Logs:        ${chalk.cyan(`kars logs ${options.name} -f`)}`);
  console.log(`  Egress:      ${chalk.cyan(`kars egress ${options.name}`)}`);

  if (webUiUrl) {
    section("WebUI");
    console.log(`  ${chalk.green("→")} ${chalk.cyan.underline(webUiUrl)}`);
  }

  // Cache deployment context for subsequent commands (add, status, list, push,
  // etc.). Setting phase: "complete" also marks the auto-resume state as fully
  // consumed so the next `kars up` starts fresh.
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

/**
 * Detect and remove DANGLING `GroundingWithBingSearch` connections on a Foundry
 * project — connections whose backing `Microsoft.Bing/accounts` resource no
 * longer exists. Such a connection (especially when `isDefault`) makes the
 * keyless, Microsoft-managed `web_search` tool fail with a misleading
 * `401 "audience is incorrect (https://bing.azure.com)"`, because Foundry tries
 * to route web_search through the dead connection instead of the managed Bing
 * resource.
 *
 * kars never provisions or stores a Bing API key (its `web_search` is keyless),
 * so the correct remediation is to delete the stale connection and let the
 * managed path take over. Strictly advisory: any error is swallowed by the
 * caller, and a connection whose Bing resource is still alive is left untouched.
 */
async function removeDanglingBingConnections(ctx: {
  execa: typeof import("execa").execa;
  stepper: Stepper;
  /** Full ARM id of the Foundry (CognitiveServices) account. */
  foundryResourceId: string;
  /** Project name (the `…/api/projects/<name>` segment). */
  foundryProjectName: string;
}): Promise<void> {
  const { execa, stepper, foundryResourceId, foundryProjectName } = ctx;
  if (!foundryResourceId || !foundryProjectName) return;

  const connectionsUrl =
    `${foundryResourceId}/projects/${foundryProjectName}/connections?api-version=2025-06-01`;
  const { stdout: connsJson } = await execa("az", [
    "rest", "--method", "get", "--url", connectionsUrl,
  ], { stdio: "pipe" }).catch(() => ({ stdout: "" }));
  if (!connsJson.trim()) return;

  let connections: Array<{
    name?: string;
    properties?: {
      category?: string;
      isDefault?: boolean;
      metadata?: { ResourceId?: string };
    };
  }> = [];
  try {
    connections = (JSON.parse(connsJson).value ?? []) as typeof connections;
  } catch {
    return;
  }

  for (const conn of connections) {
    if (conn.properties?.category !== "GroundingWithBingSearch") continue;
    const bingResourceId = conn.properties?.metadata?.ResourceId;
    const connName = conn.name;
    if (!bingResourceId || !connName) continue;

    // Does the backing Bing resource still resolve? A 404 / ResourceGroupNotFound
    // (e.g. the RG was deleted) marks the connection as dangling.
    const probe = await execa("az", [
      "rest", "--method", "get",
      "--url", `${bingResourceId}?api-version=2020-06-10`,
    ], { stdio: "pipe" }).then(() => ({ ok: true, err: "" }))
      .catch((e: { stderr?: string; message?: string }) => ({
        ok: false,
        err: String(e.stderr || e.message || ""),
      }));

    const dangling = !probe.ok &&
      /ResourceGroupNotFound|ResourceNotFound|NotFound|could not be found|status code 404/i.test(probe.err);
    if (!dangling) continue;

    // Remove the dead connection at both project and account scope (it can
    // exist at either). Deletions are idempotent — a missing one is fine.
    const projConnUrl =
      `${foundryResourceId}/projects/${foundryProjectName}/connections/${connName}?api-version=2025-06-01`;
    const acctConnUrl =
      `${foundryResourceId}/connections/${connName}?api-version=2025-06-01`;
    await execa("az", ["rest", "--method", "delete", "--url", projConnUrl], { stdio: "pipe" }).catch(() => {});
    await execa("az", ["rest", "--method", "delete", "--url", acctConnUrl], { stdio: "pipe" }).catch(() => {});

    stepper.detail(
      "info",
      `Removed dangling Bing connection '${connName}' (backing resource deleted) — ` +
        `keyless managed web_search will be used instead`,
    );
  }
}
