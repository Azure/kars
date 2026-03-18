import { Command } from "commander";
import chalk from "chalk";
import ora from "ora";

const DEFAULT_SANDBOX_IMAGE =
  "azureclaw.azurecr.io/openclaw-sandbox:latest";
const AZURELINUX4_BASE =
  "azlpubstagingacroxz2o4gw.azurecr.io/azurelinux/base/core:4.0";

export function devCommand(): Command {
  const cmd = new Command("dev");

  cmd
    .description(
      "Run a sandbox locally via Docker for development. Same policies, same model routing, on your laptop."
    )
    .option("--name <name>", "Sandbox name", "dev-agent")
    .option("--model <model>", "AI model", "gpt-4.1")
    .option(
      "--policy <preset>",
      "Policy preset: minimal, developer, web, azure",
      "developer"
    )
    .option(
      "--image <image>",
      "Sandbox container image",
      DEFAULT_SANDBOX_IMAGE
    )
    .option(
      "--build",
      "Build sandbox image locally from Dockerfile (uses Azure Linux 4 base)",
      false
    )
    .action(async (options) => {
      console.log(chalk.blue("\n🦞 AzureClaw Dev — local sandbox\n"));

      const spinner = ora("").start();

      try {
        let image = options.image;

        if (options.build) {
          // Build from local Dockerfile using Azure Linux 4 base
          spinner.text = `Building sandbox image from Azure Linux 4 (${AZURELINUX4_BASE})...`;
          const { execa } = await import("execa");
          await execa("docker", [
            "build",
            "--build-arg", `AZURELINUX_BASE=${AZURELINUX4_BASE}`,
            "-t", "azureclaw-sandbox:dev",
            "-f", "sandbox-images/openclaw/Dockerfile",
            ".",
          ], { stdio: "pipe" });
          image = "azureclaw-sandbox:dev";
        } else {
          // Pull pre-built sandbox image
          spinner.text = `Pulling sandbox image (${image})...`;
          const { execa } = await import("execa");
          try {
            await execa("docker", ["pull", image], { stdio: "pipe" });
          } catch {
            // If pull fails, try building locally
            spinner.text = `Pull failed — building locally from Azure Linux 4...`;
            await execa("docker", [
              "build",
              "--build-arg", `AZURELINUX_BASE=${AZURELINUX4_BASE}`,
              "-t", "azureclaw-sandbox:dev",
              "-f", "sandbox-images/openclaw/Dockerfile",
              ".",
            ], { stdio: "pipe" });
            image = "azureclaw-sandbox:dev";
          }
        }

        // Start container with security constraints
        spinner.text = `Starting local sandbox '${options.name}'...`;
        const containerName = `azureclaw-${options.name}`;
        const { execa: exec2 } = await import("execa");

        // Stop any existing container with the same name
        try {
          await exec2("docker", ["rm", "-f", containerName], { stdio: "pipe" });
        } catch {
          // Container didn't exist — fine
        }

        // Run the sandbox container
        await exec2("docker", [
          "run", "-d",
          "--name", containerName,
          "--security-opt", `seccomp=${process.cwd()}/policy-engine/profiles/seccomp/azureclaw-strict.json`,
          "--read-only",
          "--tmpfs", "/tmp:rw,noexec,nosuid,size=1g",
          "-v", `${containerName}-data:/sandbox`,
          "-p", "18789:18789",
          "-e", `OPENCLAW_MODEL=${options.model}`,
          image,
        ], { stdio: "pipe" });

        spinner.succeed("Local sandbox ready!");

        console.log(
          `\n  Base:   ${chalk.bold("Azure Linux 4")} (same OS as AKS production)`
        );
        console.log(
          `  Model:  ${chalk.bold(options.model)} (via your local Azure credentials)`
        );
        console.log(
          `  Policy: ${chalk.bold(options.policy)} preset`
        );
        console.log(
          `\n  Connect: ${chalk.cyan(`azureclaw ${options.name} connect`)}`
        );
        console.log(
          chalk.dim(
            `\n  When ready for production: azureclaw deploy\n`
          )
        );
      } catch (error) {
        spinner.fail("Local sandbox failed to start");
        const message =
          error instanceof Error ? error.message : String(error);
        console.error(chalk.red(`\nError: ${message}\n`));
        process.exit(1);
      }
    });

  return cmd;
}
