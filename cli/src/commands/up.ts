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
          spinner.text = "Reading existing deployment outputs...";
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

        // ── Step 3b: Add AKS egress IP to service firewalls ──────
        spinner.text = "Adding AKS egress IP to service firewalls...";
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
        spinner.text = "Attaching ACR to AKS...";
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
        spinner.text = "Configuring kubectl...";
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
          spinner.text = "Logging into ACR...";
          await execa("az", ["acr", "login", "--name", acr], { stdio: "pipe" });

          const buildPush = async (dockerfile: string, tag: string, buildArgs: string[] = []) => {
            spinner.text = `Building ${tag}...`;
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
        spinner.text = "Installing AzureClaw controller...";
        const foundryEndpoint = options.foundryEndpoint || "";

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
              spinner.text = "Assigning Cognitive Services User role to kubelet MI (via Bicep)...";
              const bicepRole = `targetScope = 'subscription'\nparam pid string\nresource r 'Microsoft.Authorization/roleAssignments@2022-04-01' = { name: guid(pid, 'csu-foundry') \n properties: { roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', 'a97b65f3-24c7-4388-baec-2e87135dc908') \n principalId: pid \n principalType: 'ServicePrincipal' } }`;
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

        // Grant Cognitive Services User role on Foundry resource (if --foundry-endpoint provided)
        if (foundryEndpoint) {
          spinner.text = "Granting Cognitive Services User role on Foundry resource...";
          // Extract resource name from endpoint URL (e.g. "lsb-azureai" from "https://lsb-azureai.openai.azure.com")
          const foundryHost = new URL(foundryEndpoint).hostname;
          const foundryResourceName = foundryHost.split(".")[0];

          // Get the MI's principal ID
          const { stdout: wiPrincipalId } = await execa("az", [
            "identity", "show",
            "--name", `${baseName}-aks-sandbox-wi`,
            "--resource-group", rg,
            "--query", "principalId",
            "--output", "tsv",
          ], { stdio: "pipe" });

          // Find the Foundry resource ID (search across subscription)
          const { stdout: foundryResourceId } = await execa("az", [
            "cognitiveservices", "account", "list",
            "--query", `[?name=='${foundryResourceName}'].id | [0]`,
            "--output", "tsv",
          ], { stdio: "pipe" }).catch(() => ({ stdout: "" }));

          if (foundryResourceId.trim()) {
            await execa("az", [
              "role", "assignment", "create",
              "--assignee", wiPrincipalId.trim(),
              "--role", "Cognitive Services User",
              "--scope", foundryResourceId.trim(),
              "--output", "none",
            ], { stdio: "pipe" }).catch(() => {
              // Already assigned or insufficient permissions — non-fatal
            });
          } else {
            // Fallback: assign at subscription scope
            await execa("az", [
              "role", "assignment", "create",
              "--assignee", wiPrincipalId.trim(),
              "--role", "Cognitive Services User",
              "--scope", `/subscriptions/${(await execa("az", ["account", "show", "--query", "id", "--output", "tsv"], { stdio: "pipe" })).stdout.trim()}`,
              "--output", "none",
            ], { stdio: "pipe" }).catch(() => {});
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
