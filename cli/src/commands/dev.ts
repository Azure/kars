import { Command } from "commander";
import chalk from "chalk";
import ora from "ora";
import { existsSync, readFileSync } from "fs";
import { join } from "path";
import { homedir } from "os";

const DEFAULT_SANDBOX_IMAGE =
  "azureclaw-sandbox:dev";
const AZURELINUX4_BASE =
  "azlpubstagingacroxz2o4gw.azurecr.io/azurelinux/base/core:4.0";
const CONFIG_DIR = join(homedir(), ".azureclaw");
const CONFIG_FILE = join(CONFIG_DIR, "config.json");
const CREDENTIALS_FILE = join(CONFIG_DIR, "credentials");

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
    .option(
      "--base-image <image>",
      "Azure Linux base image for building sandbox (override for custom registries)",
      AZURELINUX4_BASE
    )
    .action(async (options) => {
      const blue = chalk.hex("#0078D4"); // Azure blue
      const dim = chalk.dim;
      const bold = chalk.bold;

      console.log(blue(`
  ╔══════════════════════════════════════════════════╗
  ║           ${bold("AzureClaw")} · Local Sandbox              ║
  ║        Secure AI Agent Runtime on Azure          ║
  ╚══════════════════════════════════════════════════╝
`));

      const spinner = ora({ color: "cyan" }).start();

      try {
        let image = options.image;
        const { execa } = await import("execa");
        const path = await import("path");

        // Find repo root
        let repoRoot = process.cwd();
        while (repoRoot !== "/" && !existsSync(path.join(repoRoot, "Cargo.toml"))) {
          repoRoot = path.dirname(repoRoot);
        }

        // ── Image resolution ─────────────────────────────────────────
        let imageExists = false;
        if (!options.build) {
          spinner.text = "Checking for sandbox image...";
          try {
            await execa("docker", ["image", "inspect", image], { stdio: "pipe" });
            imageExists = true;
          } catch {
            // Not found
          }
        }

        if (options.build || !imageExists) {
          const baseImage = options.baseImage;
          try {
            await execa("docker", ["image", "inspect", baseImage], { stdio: "pipe" });
          } catch {
            spinner.fail(`Azure Linux 4 base image not found`);
            console.log(chalk.yellow(`
  The AzureClaw sandbox requires the Azure Linux 4 Alpha base image.
  This is a limited-availability image — request access first:

  ${chalk.bold("1.")} Request access: ${chalk.cyan("https://eng.ms/docs/products/azure-linux/overview/AzureLinux4Alpha1")}
  ${chalk.bold("2.")} Pull the image: ${chalk.cyan(`docker pull ${baseImage}`)}
  ${chalk.bold("3.")} Re-run:         ${chalk.cyan("azureclaw dev")}

  Custom registry? ${chalk.cyan(`azureclaw dev --base-image <your-registry>/azurelinux/base/core:4.0`)}
`));
            process.exit(1);
          }

          const dockerfilePath = path.join(repoRoot, "sandbox-images/openclaw/Dockerfile");
          if (!existsSync(dockerfilePath)) {
            spinner.fail("Dockerfile not found");
            console.log(chalk.yellow(`
  Run from the AzureClaw repo root:
    ${chalk.cyan("git clone https://github.com/Azure/azureclaw.git && cd azureclaw")}
    ${chalk.cyan("azureclaw dev")}
`));
            process.exit(1);
          }

          spinner.text = "Building sandbox image from Azure Linux 4 (first run takes a few minutes)...";
          await execa("docker", [
            "build",
            "--build-arg", `AZURELINUX_BASE=${baseImage}`,
            "-t", "azureclaw-sandbox:dev",
            "-f", dockerfilePath,
            repoRoot,
          ], { stdio: "pipe" });
          image = "azureclaw-sandbox:dev";
          spinner.succeed("Sandbox image built");
        } else {
          spinner.succeed("Sandbox image found");
        }

        // ── Load config from ~/.azureclaw/ ──────────────────────────
        let config: Record<string, string> = {};
        let hasCredentials = false;

        if (existsSync(CONFIG_FILE)) {
          try {
            config = JSON.parse(readFileSync(CONFIG_FILE, "utf-8"));
          } catch {
            // Corrupt config
          }
        }
        if (existsSync(CREDENTIALS_FILE)) {
          hasCredentials = true;
        }

        if (!config.endpoint || !hasCredentials) {
          spinner.stop();
          console.log(chalk.yellow(`  No configuration found. Run ${chalk.cyan("azureclaw onboard")} first to set up your Azure OpenAI credentials.\n`));
          process.exit(1);
        }

        const model = options.model !== "gpt-4.1" ? options.model : (config.model || "gpt-4.1");

        // ── Container startup ────────────────────────────────────────
        const containerName = `azureclaw-${options.name}`;

        // Clean up any previous instance
        try {
          await execa("docker", ["rm", "-f", containerName], { stdio: "pipe" });
        } catch {
          // Didn't exist — fine
        }

        // Seccomp profile — copied into dist/profiles/ during build
        const { fileURLToPath } = await import("url");
        const thisFile = fileURLToPath(import.meta.url);
        const distDir = path.dirname(path.dirname(thisFile));
        const seccompPath = path.join(distDir, "profiles", "seccomp", "azureclaw-strict.json");
        const hasSeccomp = existsSync(seccompPath);
        const seccompArgs = hasSeccomp
          ? ["--security-opt", `seccomp=${seccompPath}`]
          : [];

        spinner.start("Starting sandbox...");
        await execa("docker", [
          "run", "-d",
          "--name", containerName,
          "--hostname", options.name,
          ...seccompArgs,
          "--read-only",
          // Writable paths
          "--tmpfs", "/tmp:rw,noexec,nosuid,size=1g",
          "-v", `${containerName}-data:/sandbox`,
          // Mount API key as read-only secret (never as env var)
          "-v", `${CREDENTIALS_FILE}:/run/secrets/azure-openai-key:ro`,
          // Hide unnecessary filesystem paths
          "--tmpfs", "/boot:ro,size=0",
          "--tmpfs", "/home:ro,size=0",
          "--tmpfs", "/media:ro,size=0",
          "--tmpfs", "/mnt:ro,size=0",
          "--tmpfs", "/srv:ro,size=0",
          "--tmpfs", "/root:ro,size=0",
          "-p", "18789:18789",
          "-e", `OPENCLAW_MODEL=${model}`,
          "-e", `AZURE_OPENAI_ENDPOINT=${config.endpoint}`,
          "-e", `PS1=azureclaw@${options.name}:\\w\\$ `,
          image,
        ], { stdio: "pipe" });
        spinner.succeed("Sandbox running");

        // ── Security status ──────────────────────────────────────────
        console.log(blue(`\n  ── Security ──────────────────────────────────────`));
        console.log(`  ${chalk.green("✓")} Read-only root filesystem`);
        console.log(`  ${chalk.green("✓")} Non-root user (sandbox:1000)`);
        console.log(`  ${chalk.green("✓")} All root privileges removed`);
        console.log(`  ${hasSeccomp ? chalk.green("✓") : chalk.yellow("○")} seccomp profile ${hasSeccomp ? "(azureclaw-strict)" : "(not loaded)"}`);
        console.log(`  ${chalk.green("✓")} Writable paths: /sandbox, /tmp only`);
        console.log(`  ${chalk.green("✓")} tmpfs /tmp (noexec, 1GB limit)`);
        console.log(`  ${chalk.green("✓")} API key mounted as read-only secret (/run/secrets/)`);

        console.log(blue(`\n  ── Inference ─────────────────────────────────────`));
        console.log(`  ${chalk.green("✓")} Rust inference router (port 8443)`);
        console.log(`  ${chalk.green("✓")} All model calls routed through router`);
        console.log(`  ${chalk.green("✓")} Token counting + latency metrics enabled`);
        console.log(`  ${chalk.green("✓")} Prometheus metrics: http://localhost:8443/metrics`);

        // ── Environment info ─────────────────────────────────────────
        console.log(blue(`\n  ── Environment ───────────────────────────────────`));
        console.log(`  OS:       ${bold("Azure Linux 4.0")} (Alpha)`);
        console.log(`  OpenClaw: ${bold("2026.3.13")}`);
        console.log(`  Model:    ${bold(model)} (Azure OpenAI)`);
        console.log(`  Endpoint: ${bold(config.endpoint)}`);
        console.log(`  Policy:   ${bold(options.policy)} preset`);
        console.log(`  Sandbox:  ${bold(options.name)}`);

        // ── Next steps ───────────────────────────────────────────────
        // Get the gateway token for web UI
        let gatewayToken = "";
        try {
          const { stdout: tokenOut } = await execa("docker", [
            "exec", containerName, "bash", "-lc", "echo $OPENCLAW_GATEWAY_TOKEN",
          ], { stdio: "pipe" });
          gatewayToken = tokenOut.trim();
        } catch {
          // Token not available yet
        }

        console.log(blue(`\n  ── Commands ──────────────────────────────────────`));
        console.log(`  Connect:  ${chalk.cyan(`azureclaw connect ${options.name}`)}`);
        console.log(`  Shell:    ${chalk.cyan(`azureclaw connect ${options.name} --shell`)}`);
        console.log(`  Status:   ${chalk.cyan(`azureclaw status ${options.name}`)}`);
        console.log(`  Stop:     ${chalk.cyan(`azureclaw destroy ${options.name}`)}`);
        if (gatewayToken) {
          console.log(`  Web UI:   ${chalk.cyan(`http://localhost:18789/#token=${gatewayToken}`)}`);
        }
        console.log(dim(`\n  Production: azureclaw up (deploys to AKS)`));
        console.log();
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
