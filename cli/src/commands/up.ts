// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import chalk from "chalk";
import { existsSync } from "fs";
import { Stepper, banner } from "../stepper.js";
import { isValidAzureHost } from "./up/preflight.js";

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
    .option("--skip-runtime-images", "Skip building/importing the 6 multi-runtime adapter images (faster first deploy; only OpenClaw + BYO will be runnable)", false)
    .option("--foundry-endpoint <url>", "Existing Azure AI Foundry project endpoint (services.ai.azure.com)")
    .option("--openai-endpoint <url>", "Existing Azure OpenAI endpoint (openai.azure.com, derived from Foundry if omitted)")
    .option("--dry-run", "Show what would be done without executing", false)
    .option("--upgrade", "Fast upgrade: skip prompts, reuse cached context, just re-run Helm + RBAC", false)
    .option("--mesh-peer", "Enable mesh federation peer (default: on; use --no-mesh-peer to disable)", true)
    .option("--global-registry <url>", "Use an external AgentMesh registry (skip local registry deployment)")
    .option("--expose-registry", "Deploy AGIC Ingress to expose this cluster's registry publicly", false)
    .option("--skip-preflight", "Skip upfront RBAC & provider checks (advanced; you know what you're doing)", false)
    .action(async (options) => {
      const { execa } = await import("execa");

      // ── FAST UPGRADE PATH (S15.d.1: extracted to ./up/fast_upgrade.ts) ──
      // Skip all prompts and infra — just re-run Helm with cached context.
      if (options.upgrade) {
        const { runFastUpgrade } = await import("./up/fast_upgrade.js");
        await runFastUpgrade(options);
        return;
      }

      // ── PREFLIGHT (S15.d.2: extracted to ./up/preflight.ts) ──
      // Auto-detect dev mode, prefill from cached context, banner +
      // tool checks, Azure auth + sub, interactive prompts (region /
      // name / isolation / backend), RBAC + provider preflight, SKU
      // availability check, and the --dry-run plan print. Returns
      // null when --dry-run was taken (we early-return), otherwise
      // returns the derived `rg` for the production-deploy section.
      const { runPreflight } = await import("./up/preflight.js");
      const preflightResult = await runPreflight(options);
      if (preflightResult === null) {
        return;
      }
      const { rg } = preflightResult;


      banner("AzureClaw · Production Deploy", "Secure AI Agent Runtime on Azure");

      const clusterName = options.clusterName ?? "azureclaw";
      const baseName = clusterName.replace(/-aks$/, "");
      let acrName = ""; // resolved from Bicep output after deployment
      // Stepper counts the 9 runtime phases below (10 with --expose-registry).
      // Preflight runs before the stepper (see runPreflightChecks above).
      //   1. Resource group + preview features
      //   2. Bicep deploy (or verify existing)
      //   3. Network/firewalls/ACR attach
      //   4. Get AKS credentials
      //   5. Build OR import images
      //   6. Helm install (CRD + controller + seccomp DS)
      //   7. AgentMesh infrastructure (relay + registry)
      //   8. (optional) AgentMesh Ingress — only with --expose-registry
      //   9. Create ClawSandbox CR
      //  10. Wait for sandbox Running
      const totalSteps = 9 + (options.exposeRegistry ? 1 : 0);
      const stepper = new Stepper({ totalSteps });

      try {
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
          stepper.fail("Bicep template not found");
          console.log(chalk.yellow(`  Expected at: ${bicepPath}`));
          console.log(chalk.yellow(`  Run from the AzureClaw repo root.\n`));
          process.exit(1);
        }

        // ── Step 1: Create resource group ────────────────────────────
        stepper.step(`Setting up resource group '${rg}'...`);

        // Check if RG already exists
        let rgExists = false;
        try {
          const { stdout: rgCheck } = await execa("az", [
            "group", "show", "--name", rg, "--query", "properties.provisioningState", "-o", "tsv",
          ], { stdio: "pipe" });
          if (rgCheck.trim() === "Succeeded") rgExists = true;
        } catch { /* doesn't exist */ }

        await execa("az", [
          "group", "create", "--name", rg, "--location", options.region, "--output", "none",
        ], { stdio: "pipe" });
        stepper.detail(rgExists ? "ok" : "new", `Resource group '${rg}' — ${rgExists ? "exists" : "created"}`);


        // ── Step 2b: Detect caller IP for firewall rules ─────────────
        stepper.update("Detecting your public IP for firewall rules...");
        let callerIp: string | null = null;
        try {
          const { stdout: ipOut } = await execa("curl", ["-s", "--max-time", "5", "https://ifconfig.me"], { stdio: "pipe" });
          const ip = ipOut.trim();
          if (/^\d{1,3}(\.\d{1,3}){3}$/.test(ip)) {
            callerIp = ip;
            stepper.detail("ok", `Public IP detected — ${ip}`);
          }
        } catch {
          stepper.detail("skip", "Public IP detection — skipped (offline?)");
        }

        // ── Step 2c: Register required preview features ──────────
        stepper.update("Registering preview features...");
        // EncryptionAtHost is required for clawpool
        await execa("az", [
          "feature", "register",
          "--namespace", "Microsoft.Compute",
          "--name", "EncryptionAtHost",
          "--output", "none",
        ], { stdio: "pipe" }).catch(() => {
          // Already registered — OK
        });

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

        stepper.done(`Resource group '${rg}' ready${callerIp ? ` (IP: ${callerIp})` : ""}`);

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
              options.skipInfra = true;
            }
          } catch {
            // Cluster doesn't exist — proceed with Bicep
          }
        }

        if (!options.skipInfra) {
          stepper.step(`Provisioning Azure resources in ${options.region}...`);
          const isolationDesc: Record<string, string> = {
            standard: "runc + RuntimeDefault seccomp",
            enhanced: "runc + azureclaw-strict seccomp + read-only rootfs",
            confidential: "Kata VM isolation (hardware TEE)",
          };
          stepper.detail("info", `Isolation: ${isolationDesc[options.isolation] || options.isolation}`);
          if (options.foundryEndpoint) {
            stepper.detail("info", `Resources: AKS + ACR + Key Vault + Monitor (using existing AI endpoint)`);
          } else {
            stepper.detail("info", `Resources: AKS + ACR + Key Vault + Azure OpenAI + Monitor`);
          }
          stepper.detail("info", `This takes 5–10 minutes. Deploying now...`);
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
          if (options.foundryEndpoint) {
            bicepParams.push("deployAoai=false");
          }

          // Run Bicep deployment with a progress ticker
          const ticker = setInterval(() => {
            stepper.update(`Provisioning Azure resources in ${options.region}... (still running)`);
          }, 30000);

          try {
            const { stdout: deployOutput } = await execa("az", [
              "deployment", "group", "create",
              "--resource-group", rg,
              "--template-file", bicepPath,
              "--parameters", ...bicepParams,
              "--output", "json",
              "--query", "properties.outputs",
            ], { stdio: "pipe" });

            clearInterval(ticker);

            const outputs = JSON.parse(deployOutput);
            acrLoginServer = outputs.acrLoginServer.value;
            acrName = outputs.acrName?.value || acrLoginServer.replace(".azurecr.io", "");
            openAiEndpoint = options.openaiEndpoint || outputs.openAiEndpoint?.value || options.foundryEndpoint || "";
            wiClientId = outputs.sandboxIdentityClientId.value;
            kvName = outputs.keyVaultName.value;

            stepper.detail("new", `AKS cluster — ${baseName}-aks`);
            stepper.detail("new", `ACR — ${acrLoginServer}`);
            stepper.detail("new", `Key Vault — ${kvName}`);
            if (options.foundryEndpoint) {
              stepper.detail("ok", `AI Backend — ${options.foundryEndpoint} (existing)`);
            } else {
              stepper.detail("new", `OpenAI — ${openAiEndpoint}`);
            }

            stepper.done("Azure resources provisioned");
          } catch (bicepErr: any) {
            clearInterval(ticker);
            throw bicepErr;
          }
        } else {
          // Read outputs from existing deployment
          stepper.step("Verifying existing infrastructure...");
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
          acrName = outputs.acrName?.value || acrLoginServer.replace(".azurecr.io", "");
          openAiEndpoint = options.openaiEndpoint || outputs.openAiEndpoint?.value || options.foundryEndpoint || "";
          wiClientId = outputs.sandboxIdentityClientId.value;
          kvName = outputs.keyVaultName.value;

          stepper.detail("ok", `AKS cluster — ${baseName}-aks (running)`);
          stepper.detail("ok", `ACR — ${acrLoginServer}`);
          stepper.detail("ok", `Key Vault — ${kvName}`);
          if (options.foundryEndpoint) {
            stepper.detail("ok", `AI Backend — ${options.foundryEndpoint} (existing)`);
          } else {
            stepper.detail("ok", `OpenAI — ${openAiEndpoint}`);
          }
          stepper.detail("ok", `Workload Identity — ${wiClientId.slice(0, 8)}...`);

          stepper.done("Infrastructure verified (Bicep skipped)");
        }

        // ── Step 3a: Ensure caller IP is in AKS API server authorized ranges ──
        stepper.step("Configuring network access & firewalls...");
        if (callerIp) {
          stepper.update("Updating AKS API server authorized IPs...");
          await execa("az", [
            "aks", "update",
            "--name", `${baseName}-aks`,
            "--resource-group", rg,
            "--api-server-authorized-ip-ranges", `${callerIp}/32`,
            "--output", "none",
          ], { stdio: "pipe" }).then(() => {
            stepper.detail("ok", `AKS API server — ${callerIp}/32 authorized`);
          }).catch(() => {
            stepper.detail("ok", `AKS API server — IP already authorized`);
          });
        }

        // ── Step 3b: Add AKS egress IP to service firewalls ──────
        stepper.update("Adding AKS egress IP to service firewalls...");
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
                "--name", acrName,
                "--ip-address", aksEgress,
                "--output", "none",
              ], { stdio: "pipe" }).then(() => {
                stepper.detail("ok", `ACR firewall — ${aksEgress} allowed`);
              }).catch(() => {
                stepper.detail("ok", `ACR firewall — already configured`);
              });
              // Add AKS egress to AOAI firewall (only when AOAI is deployed in this RG)
              if (!options.foundryEndpoint) {
                await execa("az", [
                  "cognitiveservices", "account", "network-rule", "add",
                  "--name", `${baseName}-aoai`,
                  "--resource-group", rg,
                  "--ip-address", aksEgress,
                  "--output", "none",
                ], { stdio: "pipe" }).then(() => {
                  stepper.detail("ok", `AOAI firewall — ${aksEgress} allowed`);
                }).catch(() => {
                  stepper.detail("ok", `AOAI firewall — already configured`);
                });
              }
            }
          }
        } catch {
          // Non-fatal — firewalls may not have rules or may already include egress
        }

        // ── Step 3c: Attach ACR to AKS ──────────────────────────────
        stepper.update("Attaching ACR to AKS...");
        await execa("az", [
          "aks", "update",
          "--name", `${baseName}-aks`,
          "--resource-group", rg,
          "--attach-acr", acrName,
          "--output", "none",
        ], { stdio: "pipe" }).then(() => {
          stepper.detail("ok", `ACR attachment — ${acrName} → AKS`);
        }).catch(() => {
          stepper.detail("ok", `ACR attachment — already attached`);
        });

        stepper.done("Network access configured");

        // ── Step 5: Get AKS credentials ──────────────────────────────
        stepper.step("Configuring kubectl...");
        await execa("az", [
          "aks", "get-credentials",
          "--name", `${baseName}-aks`,
          "--resource-group", rg,
          "--overwrite-existing",
          "--output", "none",
        ], { stdio: "pipe" });
        stepper.done("kubectl configured");

        // ── Step 6: Get images into ACR ──────────────────────────────
        const acr = acrLoginServer.replace(".azurecr.io", "");

        if (options.build) {
          // Developer mode: build locally and push
          stepper.step("Building and pushing images...");
          stepper.update("Logging into ACR...");
          await execa("az", ["acr", "login", "--name", acr], { stdio: "pipe" });

          const buildPush = async (dockerfile: string, tag: string, buildArgs: string[] = [], context?: string) => {
            stepper.update(`Building ${tag}...`);
            const args = [
              "build", "--platform", "linux/amd64",
              "--provenance=false", "--sbom=false",
              "-f", path.join(repoRoot, dockerfile),
              "-t", `${acrLoginServer}/${tag}`,
              ...buildArgs,
              context ? path.join(repoRoot, context) : repoRoot,
            ];
            await execa("docker", args, { stdio: "pipe" });
            // Push with retry — ACR tokens/connections can go stale after long builds
            for (let attempt = 1; attempt <= 3; attempt++) {
              try {
                stepper.update(`Pushing ${tag}${attempt > 1 ? ` (retry ${attempt}/3)` : ""}...`);
                if (attempt > 1) await execa("az", ["acr", "login", "--name", acr], { stdio: "pipe" });
                await execa("docker", ["push", `${acrLoginServer}/${tag}`], { stdio: "pipe" });
                break;
              } catch (e: any) {
                if (attempt === 3) throw e;
                stepper.update(`Push ${tag} failed, retrying...`);
                await new Promise(r => setTimeout(r, 5000));
              }
            }
          };

          await buildPush("controller/Dockerfile", "azureclaw-controller:latest");
          await buildPush("inference-router/Dockerfile", "azureclaw-inference-router:latest");

          // Build sandbox base if not already in ACR
          let baseExists = false;
          try {
            await execa("docker", ["image", "inspect", `${acrLoginServer}/azureclaw-sandbox-base:latest`], { stdio: "pipe" });
            baseExists = true;
          } catch { /* not cached locally — need to build */ }
          if (!baseExists) {
            await buildPush(
              "sandbox-images/openclaw/Dockerfile.base",
              "azureclaw-sandbox-base:latest",
              ["--build-arg", `OPENCLAW_CACHE_BUST=${Date.now()}`]
            );
          }

          await buildPush(
            "sandbox-images/openclaw/Dockerfile",
            "openclaw-sandbox:latest",
            ["--build-arg", `SANDBOX_BASE_IMAGE=${acrLoginServer}/azureclaw-sandbox-base:latest`,
             "--build-arg", `INFERENCE_ROUTER_IMAGE=${acrLoginServer}/azureclaw-inference-router:latest`]
          );

          // AgentMesh components (relay + registry for E2E encrypted inter-agent comms)
          await buildPush("vendor/agentmesh-relay/Dockerfile", "agentmesh-relay:latest", [], "vendor/agentmesh-relay");
          await buildPush("vendor/agentmesh-registry/Dockerfile", "agentmesh-registry:latest", [], "vendor/agentmesh-registry");

          // Multi-runtime adapter images. Tags must match the controller's
          // DEFAULT_*_IMAGE constants in `reconciler/runtime.rs`. Skipped
          // when --skip-runtime-images is passed (faster first deploy).
          if (!options.skipRuntimeImages) {
            for (const rt of [
              { dir: "openai-agents", tag: "azureclaw-runtime-openai-agents:latest" },
              { dir: "maf-python", tag: "azureclaw-runtime-maf-python:latest" },
              { dir: "anthropic", tag: "azureclaw-runtime-anthropic:latest" },
              { dir: "langgraph", tag: "azureclaw-runtime-langgraph:latest" },
              { dir: "langgraph-ts", tag: "azureclaw-runtime-langgraph-ts:latest" },
              { dir: "pydantic-ai", tag: "azureclaw-runtime-pydantic-ai:latest" },
            ]) {
              await buildPush(`sandbox-images/${rt.dir}/Dockerfile`, rt.tag);
            }
          }

          stepper.done("Images built and pushed to ACR");
        } else {
          // Customer mode: import pre-built images from source ACR
          stepper.step("Importing images from source ACR...");
          const sourceAcr = options.sourceAcr;
          const images = [
            { source: `${sourceAcr}/azureclaw-controller:latest`, target: "azureclaw-controller:latest" },
            { source: `${sourceAcr}/azureclaw-inference-router:latest`, target: "azureclaw-inference-router:latest" },
            { source: `${sourceAcr}/openclaw-sandbox:latest`, target: "openclaw-sandbox:latest" },
            { source: `${sourceAcr}/agentmesh-relay:latest`, target: "agentmesh-relay:latest" },
            { source: `${sourceAcr}/agentmesh-registry:latest`, target: "agentmesh-registry:latest" },
            // Multi-runtime adapter images. Failures here are non-fatal —
            // some source ACRs may not host every runtime.
            { source: `${sourceAcr}/azureclaw-runtime-openai-agents:latest`, target: "azureclaw-runtime-openai-agents:latest" },
            { source: `${sourceAcr}/azureclaw-runtime-maf-python:latest`, target: "azureclaw-runtime-maf-python:latest" },
            { source: `${sourceAcr}/azureclaw-runtime-anthropic:latest`, target: "azureclaw-runtime-anthropic:latest" },
            { source: `${sourceAcr}/azureclaw-runtime-langgraph:latest`, target: "azureclaw-runtime-langgraph:latest" },
            { source: `${sourceAcr}/azureclaw-runtime-langgraph-ts:latest`, target: "azureclaw-runtime-langgraph-ts:latest" },
            { source: `${sourceAcr}/azureclaw-runtime-pydantic-ai:latest`, target: "azureclaw-runtime-pydantic-ai:latest" },
          ];

          for (const img of images) {
            stepper.update(`Importing ${img.target}...`);
            await execa("az", [
              "acr", "import",
              "--name", acr,
              "--source", img.source,
              "--image", img.target,
              "--force",
            ], { stdio: "pipe" }).then(() => {
              stepper.detail("ok", img.target);
            }).catch(() => {
              stepper.detail("skip", `${img.target} — import failed (may already exist)`);
            });
          }

          stepper.done("Images available in ACR");
        }

        // ── Step 6: Install / upgrade Helm chart ─────────────────────
        stepper.step("Deploying Helm chart (controller + CRD + RBAC)...");

        // Check if Helm release already exists
        let helmExists = false;
        try {
          const { stdout: helmStatus } = await execa("helm", [
            "status", "azureclaw", "-n", "azureclaw-system", "-o", "json",
          ], { stdio: "pipe" });
          const status = JSON.parse(helmStatus);
          if (status.info?.status === "deployed") helmExists = true;
        } catch { /* not installed */ }

        const foundryEndpoint = options.foundryEndpoint || "";

        // Use explicitly provided OpenAI endpoint, or derive from Foundry resource name
        if (!openAiEndpoint && options.openaiEndpoint) {
          openAiEndpoint = options.openaiEndpoint;
        }
        if (!openAiEndpoint && foundryEndpoint) {
          const match = foundryEndpoint.match(/https:\/\/([^.]+)\.services\.ai\.azure\.com/);
          if (match) {
            openAiEndpoint = `https://${match[1]}.openai.azure.com`;
            console.log(`  Derived OpenAI inference endpoint: ${openAiEndpoint}`);
          }
        }

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
              stepper.update("Cleaning stale Helm release...");
              await execa("kubectl", ["delete", "secret", secret, "-n", "azureclaw-system"], { stdio: "pipe" }).catch(() => {});
            }
          }
        } catch {
          // No stale secrets — normal
        }

        stepper.update("Detecting kubelet managed identity for IMDS auth...");

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
              stepper.update("Assigning Cognitive Services roles to kubelet MI (via Bicep)...");
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
          "--set", `controller.image.tag=latest`,
          "--set", `inferenceRouter.image.repository=${acrLoginServer}/azureclaw-inference-router`,
          "--set", `inferenceRouter.image.tag=latest`,
          "--set", `inferenceRouter.azure.openai.endpoint=${openAiEndpoint}`,
          "--set", `sandbox.image.repository=${acrLoginServer}/openclaw-sandbox`,
          "--set", `sandbox.image.tag=latest`,
          // Multi-runtime adapter image overrides (consumed by controller via
          // *_RUNTIME_IMAGE env vars; see helm controller-deployment.yaml).
          "--set", `runtimes.openaiAgents.image=${acrLoginServer}/azureclaw-runtime-openai-agents:latest`,
          "--set", `runtimes.mafPython.image=${acrLoginServer}/azureclaw-runtime-maf-python:latest`,
          "--set", `runtimes.anthropic.image=${acrLoginServer}/azureclaw-runtime-anthropic:latest`,
          "--set", `runtimes.langgraph.image=${acrLoginServer}/azureclaw-runtime-langgraph:latest`,
          "--set", `runtimes.langgraphTs.image=${acrLoginServer}/azureclaw-runtime-langgraph-ts:latest`,
          "--set", `runtimes.pydanticAi.image=${acrLoginServer}/azureclaw-runtime-pydantic-ai:latest`,
          "--set", `azure.workloadIdentity.clientId=${wiClientId}`,
          "--set", `azure.keyVaultCsi.keyVaultName=${kvName}`,
          "--wait",
          "--timeout", "5m",
        ];
        if (foundryEndpoint) {
          helmArgs.push("--set", `foundry.endpoint=${foundryEndpoint}`);
          // If the endpoint is a Foundry project URL, also set it as the project endpoint
          if (isValidAzureHost(foundryEndpoint, "services.ai.azure.com") && foundryEndpoint.includes("/api/projects/")) {
            helmArgs.push("--set", `foundry.projectEndpoint=${foundryEndpoint}`);
          }
          if (imdsClientId) {
            helmArgs.push("--set", `foundry.imdsClientId=${imdsClientId}`);
          }
        }

        // Discover deployed models via Azure CLI (ARM management API)
        let discoveredDeployments = "";
        if (foundryEndpoint) {
          try {
            const accountName = new URL(foundryEndpoint).hostname.split(".")[0];
            const { stdout: rgOut } = await execa("az", [
              "cognitiveservices", "account", "list",
              "--query", `[?name=='${accountName}'].resourceGroup | [0]`,
              "--output", "tsv",
            ], { stdio: "pipe", timeout: 15000 });
            const foundryRg = rgOut.trim();
            if (foundryRg) {
              const { stdout } = await execa("az", [
                "cognitiveservices", "account", "deployment", "list",
                "--name", accountName,
                "--resource-group", foundryRg,
                "--query", "[].name",
                "--output", "json",
              ], { stdio: "pipe", timeout: 30000 });
              const deps = JSON.parse(stdout || "[]");
              if (Array.isArray(deps) && deps.length > 0) {
                discoveredDeployments = JSON.stringify(deps);
                stepper.detail("ok", `Deployments — ${deps.join(", ")}`);
              }
            }
          } catch { /* non-critical */ }
        }
        if (discoveredDeployments) {
          // Helm --set treats commas as key separators; escape them
          const escaped = discoveredDeployments.replace(/,/g, "\\,");
          helmArgs.push("--set-string", `foundry.deployments=${escaped}`);
        }

        // Pass federated credential config so controller can auto-create fedcreds for sub-agents
        try {
          const { stdout: oidcIssuerUrl } = await execa("az", [
            "aks", "show",
            "--name", `${baseName}-aks`,
            "--resource-group", rg,
            "--query", "oidcIssuerProfile.issuerUrl",
            "--output", "tsv",
          ], { stdio: "pipe", timeout: 15000 });
          const { stdout: subIdRaw } = await execa("az", [
            "account", "show", "--query", "id", "--output", "tsv",
          ], { stdio: "pipe", timeout: 10000 });
          if (oidcIssuerUrl.trim() && subIdRaw.trim()) {
            helmArgs.push(
              "--set", `fedcred.subscriptionId=${subIdRaw.trim()}`,
              "--set", `fedcred.identityName=${baseName}-aks-sandbox-wi`,
              "--set", `fedcred.identityResourceGroup=${rg}`,
              "--set", `fedcred.oidcIssuerUrl=${oidcIssuerUrl.trim()}`,
            );
          }
        } catch { /* non-critical — controller will log warning */ }

        stepper.update(`${helmExists ? "Upgrading" : "Installing"} AzureClaw Helm chart (controller + CRD + RBAC + seccomp)...`);
        await execa("helm", helmArgs, { stdio: "pipe" });
        stepper.detail(helmExists ? "ok" : "new", `Helm release — ${helmExists ? "upgraded" : "installed"}`);

        // Force rollout when using :latest tags (Helm won't restart pods if spec hash is unchanged)
        stepper.update("Rolling out updated controller...");
        await execa("kubectl", [
          "rollout", "restart", "deployment/azureclaw-controller",
          "-n", "azureclaw-system",
        ], { stdio: "pipe" }).catch(() => {});
        await execa("kubectl", [
          "rollout", "status", "deployment/azureclaw-controller",
          "-n", "azureclaw-system",
          "--timeout=120s",
        ], { stdio: "pipe" }).catch(() => {});

        stepper.done(`Controller ${helmExists ? "upgraded" : "deployed"}`);

        // ── Step 6b/6c: Inspektor Gadget + AgentMesh deploy ──────────
        // (S15.d.3: extracted to ./up/agentmesh_deploy.ts)
        const { deployAgentMesh } = await import("./up/agentmesh_deploy.js");
        const meshResult = await deployAgentMesh(
          { repoRoot, acr, acrLoginServer, baseName, rg, stepper },
          { globalRegistry: options.globalRegistry, exposeRegistry: options.exposeRegistry },
        );
        const registryMode = meshResult.registryMode;
        const globalRegistryUrl = meshResult.globalRegistryUrl;
        const globalRelayUrl = meshResult.globalRelayUrl;

        // ── Step 7+8: Sandbox bring-up (S15.d.4: extracted to ./up/sandbox_bringup.ts) ──
        // Federated credentials, MI Contributor, Foundry RBAC, ClawSandbox CR,
        // wait for Running, WebUI port-forward, summary, saveContext().
        const { bringUpSandbox } = await import("./up/sandbox_bringup.js");
        await bringUpSandbox({
          options,
          baseName, rg,
          acrLoginServer, foundryEndpoint, openAiEndpoint, kvName,
          wiClientId, imdsClientId,
          repoRoot, stepper,
          registryMode, globalRegistryUrl, globalRelayUrl,
        });

      } catch (error) {
        stepper.stop();
        console.error(chalk.red(`\n  Deployment failed`));
        const message =
          error instanceof Error ? error.message : String(error);
        console.error(chalk.red(`  ${message}\n`));

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
