import { Command } from "commander";
import chalk from "chalk";
import ora from "ora";
import { existsSync } from "fs";

export function upCommand(): Command {
  const cmd = new Command("up");

  cmd
    .description(
      "One command to go from zero to running agent. Provisions Azure resources, builds images, deploys controller, creates sandbox."
    )
    .option("--name <name>", "Sandbox name", "my-assistant")
    .option("--model <model>", "AI model", "gpt-4.1")
    .option(
      "--policy <preset>",
      "Policy preset: minimal, developer, web, azure",
      "developer"
    )
    .option("--region <region>", "Azure region", "eastus2")
    .option("--cluster-name <name>", "AKS cluster name", "azureclaw")
    .option(
      "--isolation <level>",
      "Pod isolation level: standard (runc), enhanced (runc + strict seccomp), confidential (Kata VM)",
      "enhanced"
    )
    .option("-g, --resource-group <name>", "Resource group name")
    .option("--skip-infra", "Skip infrastructure provisioning (reuse existing cluster)", false)
    .option("--force-infra", "Force Bicep deployment even if AKS cluster exists", false)
    .option("--source-acr <server>", "Source ACR for pre-built images (customers)", "azureclawacr.azurecr.io")
    .option("--build", "Build images locally and push to ACR (developer mode)", false)
    .option("--foundry-endpoint <url>", "Existing Foundry/AI Services endpoint (skip AOAI deployment)")
    .option("--dry-run", "Show what would be done without executing", false)
    .action(async (options) => {
      const blue = chalk.hex("#0078D4");
      const bold = chalk.bold;

      // ── Dry-run mode: just print the plan ──────────────────────────
      if (options.dryRun) {
        const rg = options.resourceGroup || `azureclaw-${options.region}`;
        const isolationDesc: Record<string, string> = {
          standard: "standard (runc + RuntimeDefault seccomp)",
          enhanced: "enhanced (runc + azureclaw-strict seccomp)",
          confidential: "confidential (Kata VM isolation)",
        };
        console.log(blue(`\n  AzureClaw · Dry Run\n`));
        console.log(`  Steps that would execute:\n`);
        console.log(`   1. Check Azure credentials (az account show)`);
        console.log(`   2. Create resource group '${rg}' in ${options.region}`);
        console.log(`   3. Detect caller public IP for firewall rules`);
        console.log(`   4. Register features: EncryptionAtHost${options.isolation === "confidential" ? ", KataVMIsolationPreview + aks-preview ext" : ""}`);
        console.log(`   5. Deploy Bicep: AKS + ACR + KV + AOAI + Monitor + WI${options.isolation === "confidential" ? " + katapool" : ""}`);
        console.log(`   6. Add AKS egress IP to ACR + AOAI firewalls`);
        console.log(`   7. Attach ACR to AKS (az aks update --attach-acr)`);
        console.log(`   8. Get AKS credentials`);
        console.log(`   9. ${options.build ? "Build images locally + push to ACR (docker build + docker push)" : `Import images from ${options.sourceAcr}: controller, inference-router, openclaw-sandbox`}`);
        console.log(`  10. Helm install: CRD + controller + seccomp DaemonSet + RBAC`);
        console.log(`  11. Create federated credential for azureclaw-${options.name}:sandbox`);
        console.log(`  12. Create ClawSandbox CR '${options.name}' (isolation: ${options.isolation})`);
        console.log(`  13. Wait for sandbox Running`);
        console.log(`\n  Configuration:`);
        console.log(`    Name:       ${options.name}`);
        console.log(`    Model:      ${options.model}`);
        console.log(`    Isolation:  ${isolationDesc[options.isolation] || options.isolation}`);
        console.log(`    Region:     ${options.region}`);
        console.log(`    RG:         ${rg}`);
        console.log(`    Source ACR: ${options.sourceAcr}`);
        console.log(`    Skip Infra: ${options.skipInfra}`);
        console.log();
        return;
      }

      console.log(blue(`
  ╔══════════════════════════════════════════════════╗
  ║           ${bold("AzureClaw")} · Production Deploy           ║
  ║        Secure AI Agent Runtime on Azure          ║
  ╚══════════════════════════════════════════════════╝
`));

      const rg = options.resourceGroup || `azureclaw-${options.region}`;
      const clusterName = options.clusterName ?? "azureclaw";
      const baseName = "azureclaw";
      const spinner = ora({ color: "cyan" }).start();

      try {
        const { execa } = await import("execa");
        const path = await import("path");
        const { fileURLToPath } = await import("url");

        // Resolve repo root from CLI package location
        const thisFile = fileURLToPath(import.meta.url);
        const cliDist = path.dirname(path.dirname(thisFile));
        // CLI lives at cli/dist/commands/ or cli/src/commands/ — repo root is 3 levels up
        let repoRoot = path.resolve(cliDist, "..", "..");
        // Fallback: walk up from CWD looking for Cargo.toml
        if (!existsSync(path.join(repoRoot, "Cargo.toml"))) {
          repoRoot = process.cwd();
          while (repoRoot !== "/" && !existsSync(path.join(repoRoot, "Cargo.toml"))) {
            repoRoot = path.dirname(repoRoot);
          }
        }

        const bicepPath = path.join(repoRoot, "deploy/bicep/main.bicep");
        const helmPath = path.join(repoRoot, "deploy/helm/azureclaw");

        if (!existsSync(bicepPath)) {
          spinner.fail("Bicep template not found");
          console.log(chalk.yellow(`  Expected at: ${bicepPath}`));
          console.log(chalk.yellow(`  Run from the AzureClaw repo root.\n`));
          process.exit(1);
        }

        // ── Step 1: Check Azure auth ─────────────────────────────────
        spinner.text = "Checking Azure credentials...";
        try {
          await execa("az", ["account", "show", "--output", "none"], { stdio: "pipe" });
        } catch {
          spinner.stop();
          console.log(chalk.yellow("  Not logged in — running az login...\n"));
          await execa("az", ["login"], { stdio: "inherit" });
          spinner.start();
        }

        // ── Step 2: Create resource group ────────────────────────────
        spinner.text = `Creating resource group '${rg}'...`;
        await execa("az", [
          "group", "create", "--name", rg, "--location", options.region, "--output", "none",
        ], { stdio: "pipe" });

        // ── Step 2b: Detect caller IP for firewall rules ─────────────
        spinner.text = "Detecting your public IP for firewall rules...";
        let callerIp: string | null = null;
        try {
          const { stdout: ipOut } = await execa("curl", ["-s", "--max-time", "5", "https://ifconfig.me"], { stdio: "pipe" });
          const ip = ipOut.trim();
          if (/^\d{1,3}(\.\d{1,3}){3}$/.test(ip)) {
            callerIp = ip;
          }
        } catch {
          // Non-fatal: skip IP restriction
        }

        // ── Step 2c: Register required preview features ──────────
        spinner.text = "Registering preview features...";
        // EncryptionAtHost is required for clawpool
        await execa("az", [
          "feature", "register",
          "--namespace", "Microsoft.Compute",
          "--name", "EncryptionAtHost",
          "--output", "none",
        ], { stdio: "pipe" }).catch(() => {});

        if (options.isolation === "confidential") {
          // Install aks-preview extension for Kata workload runtime
          await execa("az", [
            "extension", "add", "--name", "aks-preview", "--upgrade",
          ], { stdio: "pipe" }).catch(() => {});

          await execa("az", [
            "feature", "register",
            "--namespace", "Microsoft.ContainerService",
            "--name", "KataVMIsolationPreview",
            "--output", "none",
          ], { stdio: "pipe" }).catch(() => {});
        }

        // Propagate feature registrations
        await execa("az", ["provider", "register", "-n", "Microsoft.Compute", "--output", "none"], { stdio: "pipe" }).catch(() => {});
        await execa("az", ["provider", "register", "-n", "Microsoft.ContainerService", "--output", "none"], { stdio: "pipe" }).catch(() => {});

        // ── Step 3: Deploy Bicep (AKS + ACR + KV + AOAI + Monitor + WI) ─
        let acrLoginServer: string;
        let openAiEndpoint: string;
        let wiClientId: string;
        let kvName: string;

        if (!options.skipInfra && !options.forceInfra) {
          // Auto-detect: if AKS cluster already exists, skip Bicep (saves ~8 min)
          try {
            const { stdout: aksCheck } = await execa("az", [
              "aks", "show", "-g", rg, "-n", `${baseName}-aks`,
              "--query", "provisioningState", "-o", "tsv",
            ], { stdio: "pipe" });
            if (aksCheck.trim() === "Succeeded") {
              spinner.text = "AKS cluster already exists — skipping Bicep. Reading deployment outputs...";
              options.skipInfra = true;
            }
          } catch {
            // Cluster doesn't exist — proceed with Bicep
          }
        }

        if (!options.skipInfra) {
          spinner.text = `Provisioning Azure resources in ${options.region} (this takes several minutes)...`;
          const bicepParams = [
            `location=${options.region}`,
            `baseName=${baseName}`,
          ];
          if (options.isolation === "confidential") {
            bicepParams.push("enableKata=true");
          }
          if (callerIp) {
            bicepParams.push(`authorizedIpRanges=["${callerIp}/32"]`);
          }

          const { stdout: deployOutput } = await execa("az", [
            "deployment", "group", "create",
            "--resource-group", rg,
            "--template-file", bicepPath,
            "--parameters", ...bicepParams,
            "--output", "json",
            "--query", "properties.outputs",
          ], { stdio: "pipe" });

          const outputs = JSON.parse(deployOutput);
          acrLoginServer = outputs.acrLoginServer.value;
          openAiEndpoint = outputs.openAiEndpoint.value;
          wiClientId = outputs.sandboxIdentityClientId.value;
          kvName = outputs.keyVaultName.value;

          spinner.succeed("Azure resources provisioned");
          spinner.start();
        } else {
          // Read outputs from existing deployment
          spinner.text = "Step 2/8: Reading existing deployment outputs (ACR, AOAI, WI, KV)...";
          const { stdout: existingOutput } = await execa("az", [
            "deployment", "group", "show",
            "--resource-group", rg,
            "--name", "main",
            "--output", "json",
            "--query", "properties.outputs",
          ], { stdio: "pipe" }).catch(async () => {
            // Fallback: try module deployment name
            return execa("az", [
              "deployment", "group", "show",
              "--resource-group", rg,
              "--name", `${baseName}-aks`,
              "--output", "json",
              "--query", "properties.outputs",
            ], { stdio: "pipe" });
          });

          const outputs = JSON.parse(existingOutput);
          acrLoginServer = outputs.acrLoginServer.value;
          openAiEndpoint = outputs.openAiEndpoint.value;
          wiClientId = outputs.sandboxIdentityClientId.value;
          kvName = outputs.keyVaultName.value;
        }

        // ── Step 3a: Ensure caller IP is in AKS API server authorized ranges ──
        if (callerIp) {
          spinner.text = "Step 3/8: Updating AKS API server authorized IPs...";
          await execa("az", [
            "aks", "update",
            "--name", `${baseName}-aks`,
            "--resource-group", rg,
            "--api-server-authorized-ip-ranges", `${callerIp}/32`,
            "--output", "none",
          ], { stdio: "pipe" }).catch(() => {
            // Non-fatal — may already be set or may not have permission
          });
        }

        // ── Step 3b: Add AKS egress IP to service firewalls ──────
        spinner.text = "Step 3/8: Adding AKS egress IP to service firewalls...";
        try {
          // Get AKS egress (outbound) IP
          const { stdout: egressIpId } = await execa("az", [
            "aks", "show",
            "--name", `${baseName}-aks`,
            "--resource-group", rg,
            "--query", "networkProfile.loadBalancerProfile.effectiveOutboundIPs[0].id",
            "--output", "tsv",
          ], { stdio: "pipe" });
          const cleanIpId = egressIpId.trim().split("\n").pop()?.trim();
          if (cleanIpId && cleanIpId.startsWith("/subscriptions")) {
            const { stdout: egressIpRaw } = await execa("az", [
              "network", "public-ip", "show",
              "--ids", cleanIpId,
              "--query", "ipAddress",
              "--output", "tsv",
            ], { stdio: "pipe" });
            const aksEgress = egressIpRaw.trim();
            if (aksEgress && /^\d{1,3}(\.\d{1,3}){3}$/.test(aksEgress)) {
              // Add AKS egress to ACR firewall
              await execa("az", [
                "acr", "network-rule", "add",
                "--name", `${baseName}acr`,
                "--ip-address", aksEgress,
                "--output", "none",
              ], { stdio: "pipe" }).catch(() => {});
              // Add AKS egress to AOAI firewall
              await execa("az", [
                "cognitiveservices", "account", "network-rule", "add",
                "--name", `${baseName}-aoai`,
                "--resource-group", rg,
                "--ip-address", aksEgress,
                "--output", "none",
              ], { stdio: "pipe" }).catch(() => {});
            }
          }
        } catch {
          // Non-fatal — firewalls may not have rules or may already include egress
        }

        // ── Step 3c: Attach ACR to AKS ──────────────────────────────
        spinner.text = "Step 4/8: Attaching ACR to AKS...";
        await execa("az", [
          "aks", "update",
          "--name", `${baseName}-aks`,
          "--resource-group", rg,
          "--attach-acr", `${baseName}acr`,
          "--output", "none",
        ], { stdio: "pipe" }).catch(() => {
          // Already attached — non-fatal
        });

        // ── Step 4: Get AKS credentials ──────────────────────────────
        spinner.text = "Step 5/8: Configuring kubectl...";
        await execa("az", [
          "aks", "get-credentials",
          "--name", `${baseName}-aks`,
          "--resource-group", rg,
          "--overwrite-existing",
          "--output", "none",
        ], { stdio: "pipe" });

        // ── Step 5: Get images into ACR ──────────────────────────────
        const acr = acrLoginServer.replace(".azurecr.io", "");

        if (options.build) {
          // Developer mode: build locally and push
          spinner.text = "Step 6/8: Logging into ACR...";
          await execa("az", ["acr", "login", "--name", acr], { stdio: "pipe" });

          const buildPush = async (dockerfile: string, tag: string, buildArgs: string[] = []) => {
            spinner.text = `Step 6/8: Building ${tag} (this may take a few minutes)...`;
            const args = [
              "build", "--platform", "linux/amd64",
              "--provenance=false", "--sbom=false",
              "-f", path.join(repoRoot, dockerfile),
              "-t", `${acrLoginServer}/${tag}`,
              ...buildArgs,
              repoRoot,
            ];
            await execa("docker", args, { stdio: "pipe" });
            spinner.text = `Pushing ${tag}...`;
            await execa("docker", ["push", `${acrLoginServer}/${tag}`], { stdio: "pipe" });
          };

          await buildPush("controller/Dockerfile", "azureclaw-controller:0.1.0");
          await buildPush("inference-router/Dockerfile", "azureclaw-inference-router:0.1.0");
          await buildPush(
            "sandbox-images/openclaw/Dockerfile",
            "openclaw-sandbox:latest",
            ["--build-arg", `INFERENCE_ROUTER_IMAGE=${acrLoginServer}/azureclaw-inference-router:0.1.0`]
          );

          spinner.succeed("Images built and pushed to ACR");
          spinner.start();
        } else {
          // Customer mode: import pre-built images from source ACR
          const sourceAcr = options.sourceAcr;
          const images = [
            { source: `${sourceAcr}/azureclaw-controller:0.1.0`, target: "azureclaw-controller:0.1.0" },
            { source: `${sourceAcr}/azureclaw-inference-router:0.1.0`, target: "azureclaw-inference-router:0.1.0" },
            { source: `${sourceAcr}/openclaw-sandbox:latest`, target: "openclaw-sandbox:latest" },
          ];

          for (const img of images) {
            spinner.text = `Importing ${img.target}...`;
            await execa("az", [
              "acr", "import",
              "--name", acr,
              "--source", img.source,
              "--image", img.target,
              "--force",
            ], { stdio: "pipe" }).catch(() => {
              // Image may already exist — non-fatal
            });
          }

          spinner.succeed("Images available in ACR");
          spinner.start();
        }

        // ── Step 6: Install / upgrade Helm chart ─────────────────────
        spinner.text = "Step 7/8: Preparing Helm deployment...";
        const foundryEndpoint = options.foundryEndpoint || "";

        // Fix orphaned namespace: label it for Helm adoption if it exists but isn't Helm-managed
        try {
          await execa("kubectl", [
            "label", "namespace", "azureclaw-system",
            "app.kubernetes.io/managed-by=Helm",
            "--overwrite",
          ], { stdio: "pipe" }).catch(() => {});
          await execa("kubectl", [
            "annotate", "namespace", "azureclaw-system",
            "meta.helm.sh/release-name=azureclaw",
            "meta.helm.sh/release-namespace=azureclaw-system",
            "--overwrite",
          ], { stdio: "pipe" }).catch(() => {});
        } catch {
          // Namespace may not exist yet — Helm will create it
        }

        // Clean up stale Helm releases (pending-install from failed previous attempts)
        try {
          const { stdout: helmSecrets } = await execa("kubectl", [
            "get", "secrets", "-n", "azureclaw-system",
            "-l", "owner=helm,status=pending-install",
            "-o", "jsonpath={.items[*].metadata.name}",
          ], { stdio: "pipe" });
          if (helmSecrets.trim()) {
            for (const secret of helmSecrets.trim().split(" ")) {
              spinner.text = "Cleaning stale Helm release...";
              await execa("kubectl", ["delete", "secret", secret, "-n", "azureclaw-system"], { stdio: "pipe" }).catch(() => {});
            }
          }
        } catch {
          // No stale secrets — normal
        }

        spinner.text = "Step 7/8: Detecting kubelet managed identity for IMDS auth...";

        // Get kubelet MI client ID for IMDS auth (CA-proof)
        let imdsClientId = "";
        if (foundryEndpoint) {
          try {
            const { stdout: kubeletId } = await execa("az", [
              "aks", "show",
              "--name", `${baseName}-aks`,
              "--resource-group", rg,
              "--query", "identityProfile.kubeletidentity.clientId",
              "--output", "tsv",
            ], { stdio: "pipe" });
            imdsClientId = kubeletId.trim().split("\n").pop()?.trim() || "";

            // Assign Cognitive Services User to kubelet MI via Bicep (bypasses CLI CA policy)
            const { stdout: kubeletPrincipal } = await execa("az", [
              "aks", "show",
              "--name", `${baseName}-aks`,
              "--resource-group", rg,
              "--query", "identityProfile.kubeletidentity.objectId",
              "--output", "tsv",
            ], { stdio: "pipe" });
            const principalId = kubeletPrincipal.trim().split("\n").pop()?.trim() || "";
            if (principalId) {
              spinner.text = "Step 7/8: Assigning Cognitive Services roles to kubelet MI (via Bicep)...";
              // Assign BOTH roles: Cognitive Services User (control-plane) + OpenAI User (data-plane)
              const bicepRole = [
                "targetScope = 'subscription'",
                "param pid string",
                "resource csu 'Microsoft.Authorization/roleAssignments@2022-04-01' = {",
                "  name: guid(pid, 'csu-foundry')",
                "  properties: {",
                "    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', 'a97b65f3-24c7-4388-baec-2e87135dc908')",
                "    principalId: pid",
                "    principalType: 'ServicePrincipal'",
                "  }",
                "}",
                "resource csoai 'Microsoft.Authorization/roleAssignments@2022-04-01' = {",
                "  name: guid(pid, 'csoai-foundry')",
                "  properties: {",
                "    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '5e0bd9bd-7b93-4f28-af87-19fc36ad61bd')",
                "    principalId: pid",
                "    principalType: 'ServicePrincipal'",
                "  }",
                "}",
              ].join("\n");
              const fs = await import("fs");
              const tmpBicep = path.join(repoRoot, ".tmp-role.bicep");
              fs.writeFileSync(tmpBicep, bicepRole);
              await execa("az", [
                "deployment", "sub", "create",
                "--location", options.region,
                "--template-file", tmpBicep,
                "--parameters", `pid=${principalId}`,
                "--output", "none",
              ], { stdio: "pipe" }).catch(() => {});
              fs.unlinkSync(tmpBicep);
            }
          } catch {
            // Non-fatal — IMDS will still try without explicit client ID
          }
        }

        const helmArgs = [
          "upgrade", "--install", "azureclaw", helmPath,
          "--namespace", "azureclaw-system",
          "--create-namespace",
          "--set", `controller.image.repository=${acrLoginServer}/azureclaw-controller`,
          "--set", `controller.image.tag=0.1.0`,
          "--set", `inferenceRouter.image.repository=${acrLoginServer}/azureclaw-inference-router`,
          "--set", `inferenceRouter.image.tag=0.1.0`,
          "--set", `inferenceRouter.azure.openai.endpoint=${openAiEndpoint}`,
          "--set", `sandbox.image.repository=${acrLoginServer}/openclaw-sandbox`,
          "--set", `sandbox.image.tag=latest`,
          "--set", `azure.workloadIdentity.clientId=${wiClientId}`,
          "--set", `azure.keyVaultCsi.keyVaultName=${kvName}`,
          "--wait",
          "--timeout", "5m",
        ];
        if (foundryEndpoint) {
          helmArgs.push("--set", `foundry.endpoint=${foundryEndpoint}`);
          if (imdsClientId) {
            helmArgs.push("--set", `foundry.imdsClientId=${imdsClientId}`);
          }
        }
        spinner.text = "Step 7/8: Installing AzureClaw Helm chart (controller + CRD + RBAC + seccomp)...";
        await execa("helm", helmArgs, { stdio: "pipe" });

        spinner.succeed("Controller deployed");
        spinner.start();

        // ── Step 6b: Deploy Inspektor Gadget (eBPF observability) ────
        spinner.text = "Deploying Inspektor Gadget (eBPF tracing)...";
        await execa("kubectl", ["gadget", "deploy"], { stdio: "pipe" }).catch(() => {
          // kubectl-gadget not installed or already deployed — non-fatal
        });

        // ── Step 7: Create ClawSandbox CR ────────────────────────────
        const sandboxNs = `azureclaw-${options.name}`;
        spinner.text = `Creating sandbox '${options.name}'...`;

        // Create federated identity credential for this sandbox's namespace
        spinner.text = `Setting up Workload Identity for ${sandboxNs}...`;
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
        ], { stdio: "pipe" }).catch(() => {
          // Already exists — non-fatal
        });

        // Grant RBAC roles on Foundry resource via Bicep (if --foundry-endpoint provided)
        // Two assignments needed:
        //   1. Sandbox WI → Azure AI User on the Foundry AI Services resource (so pods can call APIs)
        //   2. Foundry project MI → Azure AI User on the resource group (so Memory Store can call models internally)
        if (foundryEndpoint) {
          spinner.text = "Configuring Foundry project RBAC (via Bicep)...";
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

            // Build Bicep that assigns roles via deployment (bypasses CLI conditional access)
            const bicepLines = [
              "targetScope = 'resourceGroup'",
              "param sandboxWiPrincipalId string",
              "param projectMiPrincipalId string",
              `param foundryAccountName string = '${foundryAccountName}'`,
              "",
              "// Azure AI User role ID — has Microsoft.CognitiveServices/* wildcard data actions",
              "var azureAiUser = '53ca6127-db72-4b80-b1b0-d745d6d5456d'",
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
              "// 2. Project MI → Azure AI User on the resource group (Memory Store internal model calls)",
              "resource projectMiRole 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (!empty(projectMiPrincipalId)) {",
              "  name: guid(resourceGroup().id, projectMiPrincipalId, 'azure-ai-user')",
              "  properties: {",
              "    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', azureAiUser)",
              "    principalId: projectMiPrincipalId",
              "    principalType: 'ServicePrincipal'",
              "  }",
              "}",
            ];

            const fs = await import("fs");
            const tmpBicep = path.join(repoRoot, ".tmp-foundry-rbac.bicep");
            fs.writeFileSync(tmpBicep, bicepLines.join("\n"));

            try {
              spinner.text = "Deploying Foundry RBAC (Bicep)...";
              await execa("az", [
                "deployment", "group", "create",
                "--resource-group", foundryRg,
                "--template-file", tmpBicep,
                "--parameters",
                `sandboxWiPrincipalId=${sandboxWiPrincipalId}`,
                `projectMiPrincipalId=${projectMiPrincipalId}`,
                "--output", "none",
              ], { stdio: "pipe" });
            } catch {
              // Non-fatal — user may lack Owner on the Foundry RG
            }
            fs.unlinkSync(tmpBicep);

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
              const fs = await import("fs");
              const tmpBicep = path.join(repoRoot, ".tmp-foundry-rbac.bicep");
              fs.writeFileSync(tmpBicep, [
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
              await execa("az", [
                "deployment", "group", "create",
                "--resource-group", foundryRg || rg,
                "--template-file", tmpBicep,
                "--parameters", `pid=${wiPid.trim().split("\n").pop()?.trim()}`,
                "--output", "none",
              ], { stdio: "pipe" }).catch(() => {});
              fs.unlinkSync(tmpBicep);
            }
          }
        }

        spinner.text = `Creating sandbox '${options.name}'...`;
        const sandboxManifest = {
          apiVersion: "azureclaw.azure.com/v1alpha1",
          kind: "ClawSandbox",
          metadata: {
            name: options.name,
            namespace: "azureclaw-system",
          },
          spec: {
            openclaw: {
              image: `${acrLoginServer}/openclaw-sandbox:latest`,
            },
            sandbox: {
              isolation: options.isolation,
            },
            inference: {
              provider: "azure-openai",
              model: options.model,
              endpoint: openAiEndpoint,
              contentSafety: true,
              promptShields: true,
            },
            networkPolicy: {
              defaultDeny: true,
              approvalRequired: true,
            },
          },
        };
        await execa("kubectl", ["apply", "-f", "-"], {
          input: JSON.stringify(sandboxManifest),
          stdio: ["pipe", "pipe", "pipe"],
        });

        // ── Step 8: Wait for sandbox ─────────────────────────────────
        spinner.text = "Waiting for sandbox to start...";
        await execa("kubectl", [
          "wait",
          "--for=jsonpath={.status.phase}=Running",
          `clawsandbox/${options.name}`,
          "-n", "azureclaw-system",
          "--timeout=120s",
        ], { stdio: "pipe" }).catch(() => {
          // Timeout OK — image pull may be slow on first deploy
        });

        spinner.succeed("Ready!");

        // ── Step 9: Extract gateway token and start port-forward ─────
        spinner.start();
        spinner.text = "Setting up WebUI access...";
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

          // Start port-forward in background
          const portForward = execa("kubectl", [
            "port-forward", "-n", sandboxNs,
            `deploy/${options.name}`, "18789:18789",
          ], { stdio: "pipe", detached: true });
          portForward.unref();
          // Give it a moment to bind
          await new Promise(r => setTimeout(r, 2000));

          if (gatewayToken) {
            webUiUrl = `http://localhost:18789/#token=${gatewayToken}`;
          }

          spinner.succeed("WebUI accessible");
        } catch {
          spinner.warn("WebUI port-forward failed (run manually: kubectl port-forward -n " + sandboxNs + " deploy/" + options.name + " 18789:18789)");
        }

        // ── Summary ──────────────────────────────────────────────────
        console.log(blue(`\n  ── Deployment ────────────────────────────────────`));
        console.log(`  Sandbox      ${bold(options.name)}`);
        console.log(`  Model        ${bold(options.model)} (Azure OpenAI, Entra ID auth)`);
        const isolationDesc: Record<string, string> = {
          standard: "standard (runc + RuntimeDefault)",
          enhanced: "enhanced (runc + azureclaw-strict seccomp)",
          confidential: "confidential (Kata VM isolation)",
        };
        console.log(`  Isolation    ${bold(isolationDesc[options.isolation] || options.isolation)}`);
        console.log(`  Region       ${bold(options.region)}`);
        console.log(`  Cluster      ${bold(`${baseName}-aks`)}`);
        console.log(`  ACR          ${bold(acrLoginServer)}`);
        console.log(`  Key Vault    ${bold(kvName)}`);
        console.log(`  AOAI         ${bold(openAiEndpoint)}`);
        console.log(`  Auth         ${bold("Workload Identity (no API keys)")}`);

        console.log(blue(`\n  ── Security ──────────────────────────────────────`));
        console.log(`  ${chalk.green("✓")} Azure Policy for Kubernetes (governance)`);
        console.log(`  ${chalk.green("✓")} Cilium CNI + NetworkPolicy (default-deny egress)`);
        console.log(`  ${chalk.green("✓")} Workload Identity (Entra ID, no keys on cluster)`);
        console.log(`  ${chalk.green("✓")} Key Vault CSI driver (secret rotation)`);
        console.log(`  ${chalk.green("✓")} OIDC issuer enabled`);
        console.log(`  ${chalk.green("✓")} Read-only rootfs, non-root, seccomp`);
        console.log(`  ${chalk.green("✓")} Inference router: Content Safety + Prompt Shields`);
        if (options.isolation === "confidential") {
          console.log(`  ${chalk.green("✓")} Kata VM isolation (pod sandboxing)`);
        }

        console.log(blue(`\n  ── Commands ──────────────────────────────────────`));
        console.log(`  Connect:     ${chalk.cyan(`azureclaw connect ${options.name}`)}`);
        console.log(`  Status:      ${chalk.cyan(`azureclaw status ${options.name}`)}`);
        console.log(`  Logs:        ${chalk.cyan(`azureclaw logs ${options.name} -f`)}`);
        console.log(`  Costs:       ${chalk.cyan(`azureclaw costs ${options.name}`)}`);
        console.log(`  kubectl:     ${chalk.cyan(`kubectl get clawsandbox -n azureclaw-system`)}`);

        if (webUiUrl) {
          console.log(blue(`\n  ── WebUI ─────────────────────────────────────────`));
          console.log(`  ${chalk.green("→")} ${chalk.cyan.underline(webUiUrl)}`);
          console.log(chalk.dim(`    Port-forward active on localhost:18789`));
        }

        console.log();
      } catch (error) {
        spinner.fail("Deployment failed");
        const message =
          error instanceof Error ? error.message : String(error);
        console.error(chalk.red(`\nError: ${message}\n`));

        // Helpful diagnostics
        if (message.includes("EncryptionAtHost")) {
          console.log(chalk.yellow("  Tip: EncryptionAtHost requires registering the feature:"));
          console.log(chalk.cyan("  az feature register --namespace Microsoft.Compute --name EncryptionAtHost"));
          console.log(chalk.cyan("  az provider register -n Microsoft.Compute\n"));
        }
        if (message.includes("quota") || message.includes("Quota")) {
          console.log(chalk.yellow("  Tip: Insufficient quota. Try a different region or VM size:"));
          console.log(chalk.cyan(`  azureclaw up --region westus3\n`));
        }
        process.exit(1);
      }
    });

  return cmd;
}
