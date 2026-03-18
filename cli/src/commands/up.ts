import { Command } from "commander";
import chalk from "chalk";
import ora from "ora";

export function upCommand(): Command {
  const cmd = new Command("up");

  cmd
    .description(
      "One command to go from zero to running agent. Provisions Azure resources, creates sandbox, connects you."
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
      "--confidential",
      "Enable Confidential Containers (SEV-SNP)",
      false
    )
    .option("-g, --resource-group <name>", "Resource group name")
    .action(async (options) => {
      console.log(chalk.blue("\n🦞 AzureClaw — starting up...\n"));

      const rg =
        options.resourceGroup || `azureclaw-${options.region}`;
      const spinner = ora("").start();

      try {
        const { execa } = await import("execa");

        // Step 1: Check Azure auth
        spinner.text = "Checking Azure credentials...";
        try {
          await execa("az", ["account", "show", "--output", "none"], {
            stdio: "pipe",
          });
        } catch {
          spinner.stop();
          console.log(
            chalk.yellow("  Not logged in — running az login...\n")
          );
          await execa("az", ["login"], { stdio: "inherit" });
          spinner.start();
        }

        // Step 2: Create resource group (idempotent)
        spinner.text = `Creating resource group '${rg}'...`;
        await execa(
          "az",
          [
            "group",
            "create",
            "--name",
            rg,
            "--location",
            options.region,
            "--output",
            "none",
          ],
          { stdio: "pipe" }
        );

        // Step 3: Check if cluster already exists
        spinner.text = "Checking for existing AzureClaw cluster...";
        let clusterExists = false;
        try {
          await execa(
            "az",
            [
              "aks",
              "show",
              "--name",
              options.clusterName ?? "azureclaw",
              "--resource-group",
              rg,
              "--output",
              "none",
            ],
            { stdio: "pipe" }
          );
          clusterExists = true;
          spinner.text = "Cluster found — skipping provisioning.";
        } catch {
          // Cluster doesn't exist — provision it
        }

        if (!clusterExists) {
          // Step 4: Deploy Bicep template
          spinner.text = `Provisioning Azure resources in ${options.region} (this takes a few minutes)...`;
          const bicepArgs = [
            "deployment",
            "group",
            "create",
            "--resource-group",
            rg,
            "--template-file",
            "deploy/bicep/main.bicep",
            "--parameters",
            `location=${options.region}`,
            "--output",
            "none",
          ];
          if (options.confidential) {
            bicepArgs.push("--parameters", "enableConfidential=true");
          }
          await execa("az", bicepArgs, { stdio: "pipe" });
        }

        // Step 5: Get AKS credentials
        spinner.text = "Configuring kubectl...";
        await execa(
          "az",
          [
            "aks",
            "get-credentials",
            "--name",
            options.clusterName ?? "azureclaw",
            "--resource-group",
            rg,
            "--overwrite-existing",
            "--output",
            "none",
          ],
          { stdio: "pipe" }
        );

        // Step 6: Install/upgrade AzureClaw Helm chart
        spinner.text = "Installing AzureClaw controller...";
        await execa(
          "helm",
          [
            "upgrade",
            "--install",
            "azureclaw",
            "deploy/helm/azureclaw",
            "--namespace",
            "azureclaw-system",
            "--create-namespace",
            "--wait",
          ],
          { stdio: "pipe" }
        );

        // Step 7: Create sandbox via ClawSandbox CRD
        spinner.text = `Creating sandbox '${options.name}' with ${options.model}...`;
        const sandboxManifest = {
          apiVersion: "azureclaw.azure.com/v1alpha1",
          kind: "ClawSandbox",
          metadata: {
            name: options.name,
            namespace: "azureclaw-system",
          },
          spec: {
            openclaw: {
              image: "azureclaw.azurecr.io/openclaw-sandbox:latest",
            },
            sandbox: {
              isolation: options.confidential
                ? "confidential"
                : "enhanced",
            },
            inference: {
              provider: "azure-openai",
              model: options.model,
              contentSafety: true,
              promptShields: true,
            },
            networkPolicy: {
              defaultDeny: true,
              approvalRequired: true,
            },
          },
        };
        await execa("kubectl", [
          "apply",
          "-f",
          "-",
        ], {
          input: JSON.stringify(sandboxManifest),
          stdio: ["pipe", "pipe", "pipe"],
        });

        // Step 8: Wait for sandbox to be ready
        spinner.text = "Waiting for sandbox to start...";
        await execa("kubectl", [
          "wait",
          "--for=jsonpath={.status.phase}=Running",
          `clawsandbox/${options.name}`,
          "-n", "azureclaw-system",
          "--timeout=120s",
        ], { stdio: "pipe" }).catch(() => {
          // Timeout is OK — sandbox may still be pulling image
        });

        spinner.succeed("Ready!");

        // Print connection info
        console.log(
          chalk.green(
            "\n──────────────────────────────────────────────────"
          )
        );
        console.log(
          `  Sandbox      ${chalk.bold(options.name)}`
        );
        console.log(
          `  Model        ${chalk.bold(options.model)} (Azure OpenAI, Managed Identity)`
        );
        console.log(
          `  Policy       ${chalk.bold(options.policy)} preset`
        );
        console.log(
          `  Region       ${chalk.bold(options.region)}`
        );
        console.log(
          chalk.green(
            "──────────────────────────────────────────────────"
          )
        );
        console.log(
          `\n  Connect:     ${chalk.cyan(`azureclaw ${options.name} connect`)}`
        );
        console.log(
          `  Status:      ${chalk.cyan(`azureclaw ${options.name} status`)}`
        );
        console.log(
          `  Logs:        ${chalk.cyan(`azureclaw ${options.name} logs -f`)}`
        );
        console.log(
          `  Costs:       ${chalk.cyan(`azureclaw ${options.name} costs`)}`
        );
        console.log();
      } catch (error) {
        spinner.fail("Setup failed");
        const message =
          error instanceof Error ? error.message : String(error);
        console.error(chalk.red(`\nError: ${message}\n`));
        process.exit(1);
      }
    });

  return cmd;
}
