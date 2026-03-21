import { Command } from "commander";
import chalk from "chalk";
import ora from "ora";

export function addCommand(): Command {
  const cmd = new Command("add");

  cmd
    .description("Add a new sandboxed agent to an existing AzureClaw cluster")
    .argument("<name>", "Name for the new sandbox agent")
    .option("--model <model>", "AI model", "gpt-4.1")
    .option("--isolation <level>", "Isolation level: standard | enhanced | confidential", "enhanced")
    .option("--token-budget-daily <tokens>", "Daily token budget (0 = unlimited)", "0")
    .option("--token-budget-per-request <tokens>", "Per-request token limit (0 = unlimited)", "0")
    .option("--agent-instructions <instructions>", "System prompt for Foundry agent")
    .option("--agent-tools <tools>", "Foundry tools: file_search,web_search,code_interpreter (comma-separated)")
    .option("--image <image>", "Custom sandbox image (default: from Helm values)")
    .option("--governance", "Enable AGT governance (tool policy, trust, audit)", false)
    .option("--trust-threshold <score>", "AGT trust threshold (0-1000, default: 500)", "500")
    .option("--policy-profile <profile>", "AGT policy profile name", "default")
    .option("--learn-egress", "Enable egress learn mode: observe all domains (blocklist still enforced), then review with 'azureclaw policy learn'", false)
    .option("--dry-run", "Print the ClawSandbox YAML without applying", false)
    .action(async (name: string, options) => {
      const { execa } = await import("execa");

      const sandbox: Record<string, unknown> = {
        apiVersion: "azureclaw.azure.com/v1alpha1",
        kind: "ClawSandbox",
        metadata: {
          name,
          namespace: "azureclaw-system",
        },
        spec: {
          openclaw: {
            version: "2026.3.13",
            ...(options.image ? { image: options.image } : {}),
            config: {
              agent: {
                model: `azure/${options.model}`,
              },
            },
          },
          sandbox: {
            isolation: options.isolation,
            seccompProfile: options.isolation === "standard" ? "RuntimeDefault" : "azureclaw-strict",
            readOnlyRootFilesystem: true,
            runAsNonRoot: true,
            allowPrivilegeEscalation: false,
            writablePaths: ["/sandbox", "/tmp"],
          },
          inference: {
            provider: "azure-ai-foundry",
            model: options.model,
            contentSafety: true,
            promptShields: true,
            tokenBudget: {
              daily: parseInt(options.tokenBudgetDaily) || 0,
              perRequest: parseInt(options.tokenBudgetPerRequest) || 0,
            },
          },
          networkPolicy: {
            defaultDeny: true,
            approvalRequired: true,
            allowedEndpoints: [
              { host: "github.com", port: 443 },
              { host: "api.github.com", port: 443 },
            ],
          },
          resources: {
            requests: { cpu: "500m", memory: "1Gi" },
            limits: { cpu: "2", memory: "4Gi" },
          },
        },
      };

      // Add Foundry agent config if provided
      if (options.agentInstructions || options.agentTools) {
        const agentSpec: Record<string, unknown> = {};
        if (options.agentInstructions) {
          agentSpec.instructions = options.agentInstructions;
        }
        if (options.agentTools) {
          agentSpec.tools = options.agentTools.split(",").map((t: string) => t.trim());
        }
        (sandbox.spec as Record<string, unknown>).agent = agentSpec;
      }

      // Add AGT governance config if enabled
      if (options.governance) {
        (sandbox.spec as Record<string, unknown>).governance = {
          enabled: true,
          toolPolicy: options.policyProfile || "default",
          trustThreshold: parseInt(options.trustThreshold) || 500,
        };
      }

      // Egress learn mode
      if (options.learnEgress) {
        const np = (sandbox.spec as Record<string, unknown>).networkPolicy as Record<string, unknown>;
        np.learnEgress = true;
      }

      const yaml = JSON.stringify(sandbox, null, 2);

      if (options.dryRun) {
        console.log(chalk.bold("\nClawSandbox manifest (dry-run):\n"));
        console.log(yaml);
        console.log(chalk.dim("\nApply with: kubectl apply -f <file>"));
        return;
      }

      const spinner = ora(`Creating sandbox '${name}' (${options.isolation}, ${options.model})...`).start();

      try {
        // Verify cluster is reachable
        await execa("kubectl", ["get", "crd", "clawsandboxes.azureclaw.azure.com"], { stdio: "pipe" });

        // Apply the ClawSandbox CRD
        await execa("kubectl", ["apply", "-f", "-"], {
          input: JSON.stringify(sandbox),
          stdio: ["pipe", "pipe", "pipe"],
        });

        // Create federated credential for the new sandbox namespace
        const namespace = `azureclaw-${name}`;
        try {
          const { stdout: aksOidc } = await execa("kubectl", [
            "get", "sa", "-n", "azureclaw-system", "azureclaw-controller",
            "-o", "jsonpath={.metadata.annotations.azure\\.workload\\.identity/client-id}",
          ], { stdio: "pipe" });

          if (aksOidc) {
            spinner.text = "Creating federated credential...";
            // Best-effort — may already exist or may not have permissions
            await execa("az", [
              "identity", "federated-credential", "create",
              "--name", `azureclaw-${name}`,
              "--identity-name", "azureclaw-identity",
              "--resource-group", "azureclaw-eastus2",
              "--issuer", "$(az aks show -g azureclaw-eastus2 -n azureclaw --query oidcIssuerProfile.issuerUrl -o tsv)",
              "--subject", `system:serviceaccount:${namespace}:sandbox`,
              "--audience", "api://AzureADTokenExchange",
            ], { stdio: "pipe", shell: true }).catch(() => {
              // Non-fatal — controller creates SA, but federated cred may need manual creation
            });
          }
        } catch {
          // Non-fatal
        }

        spinner.succeed(`Sandbox '${name}' created`);
        console.log(chalk.dim(`  Namespace:  ${namespace}`));
        console.log(chalk.dim(`  Model:      ${options.model}`));
        console.log(chalk.dim(`  Isolation:  ${options.isolation}`));
        console.log(chalk.dim(`  Status:     kubectl get clawsandbox ${name} -n azureclaw-system`));
        console.log(chalk.dim(`  Connect:    azureclaw connect ${name}`));
        console.log(chalk.dim(`  Remove:     azureclaw destroy ${name}\n`));

      } catch (error) {
        spinner.fail("Failed to create sandbox");
        const message = error instanceof Error ? error.message : String(error);
        if (message.includes("clawsandboxes.azureclaw.azure.com")) {
          console.error(chalk.red("\n  AzureClaw is not installed on this cluster."));
          console.error(chalk.red("  Run 'azureclaw up' first to deploy the infrastructure.\n"));
        } else {
          console.error(chalk.red(`\n  Error: ${message}\n`));
        }
      }
    });

  return cmd;
}
