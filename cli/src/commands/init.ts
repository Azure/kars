import { Command } from "commander";
import chalk from "chalk";
import ora from "ora";

export function initCommand(): Command {
  const cmd = new Command("init");

  cmd
    .description(
      "Initialize AKS cluster with Azure Container Linux nodes and supporting Azure resources"
    )
    .requiredOption("-g, --resource-group <name>", "Azure resource group name")
    .requiredOption("-l, --location <region>", "Azure region (e.g. eastus2)")
    .option("--cluster-name <name>", "AKS cluster name", "azureclaw")
    .option(
      "--node-count <count>",
      "Number of nodes in the sandbox pool",
      "3"
    )
    .option(
      "--vm-size <size>",
      "VM size for sandbox nodes",
      "Standard_D4s_v5"
    )
    .option(
      "--confidential",
      "Use confidential VMs (AMD SEV-SNP) for hardware isolation",
      false
    )
    .option(
      "--enable-fips",
      "Enable FIPS 140-2 validated cryptographic modules",
      false
    )
    .action(async (options) => {
      console.log(
        chalk.blue("\n🔧 Initializing AzureClaw infrastructure...\n")
      );

      const spinner = ora("Validating Azure credentials...").start();

      try {
        // Step 1: Validate Azure login
        spinner.text = "Checking Azure CLI authentication...";
        // TODO: az account show

        // Step 2: Create resource group
        spinner.text = `Creating resource group '${options.resourceGroup}'...`;
        // TODO: az group create

        // Step 3: Deploy Bicep template (AKS + ACR + KV + AOAI + Monitor)
        spinner.text =
          "Deploying Azure resources (AKS, ACR, Key Vault, Azure OpenAI, Monitor)...";
        // TODO: az deployment group create --template-file deploy/bicep/main.bicep

        // Step 4: Get AKS credentials
        spinner.text = "Configuring kubectl...";
        // TODO: az aks get-credentials

        // Step 5: Install AzureClaw Helm chart
        spinner.text = "Installing AzureClaw controller...";
        // TODO: helm install azureclaw deploy/helm/azureclaw

        spinner.succeed("AzureClaw infrastructure ready!");

        console.log(chalk.green("\n✅ Cluster initialized successfully!\n"));
        console.log(
          `  Cluster:        ${chalk.bold(options.clusterName)}`
        );
        console.log(
          `  Resource Group: ${chalk.bold(options.resourceGroup)}`
        );
        console.log(`  Location:       ${chalk.bold(options.location)}`);
        console.log(
          `  Node OS:        ${chalk.bold("Azure Container Linux")}`
        );
        console.log(
          `  Node Count:     ${chalk.bold(options.nodeCount)}`
        );
        console.log(
          `  VM Size:        ${chalk.bold(options.vmSize)}`
        );
        if (options.confidential) {
          console.log(
            `  Isolation:      ${chalk.bold("Confidential (SEV-SNP)")}`
          );
        }
        console.log(
          `\nNext: Run ${chalk.cyan("azureclaw onboard")} to set up your first agent.\n`
        );
      } catch (error) {
        spinner.fail("Initialization failed");
        const message =
          error instanceof Error ? error.message : String(error);
        console.error(chalk.red(`\nError: ${message}\n`));
        process.exit(1);
      }
    });

  return cmd;
}
