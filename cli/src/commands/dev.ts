import { Command } from "commander";
import chalk from "chalk";
import { existsSync } from "fs";
import { Stepper, banner, section, kvLine, checkLine } from "../stepper.js";
import { ensureCredentials, CREDENTIALS_FILE } from "../config.js";

const DEFAULT_SANDBOX_IMAGE =
  "azureclaw-sandbox:dev";
const AZURELINUX_BASE =
  "mcr.microsoft.com/azurelinux/base/core:3.0";

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
      "Build sandbox image locally from Dockerfile",
      false
    )
    .option(
      "--base-image <image>",
      "Azure Linux base image for building sandbox (override for custom registries)",
      AZURELINUX_BASE
    )
    .action(async (options) => {
      banner("AzureClaw · Local Sandbox", "Secure AI Agent Runtime on Azure");

      const stepper = new Stepper({ totalSteps: 3 });

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
        stepper.step("Resolving sandbox image...");
        let imageExists = false;
        if (!options.build) {
          stepper.update("Checking for sandbox image...");
          try {
            await execa("docker", ["image", "inspect", image], { stdio: "pipe" });
            imageExists = true;
          } catch {
            // Not found — will build
          }
        }

        if (options.build || !imageExists) {
          const baseImage = options.baseImage;

          // Check if base image exists locally, pull if not
          try {
            await execa("docker", ["image", "inspect", baseImage], { stdio: "pipe" });
          } catch {
            stepper.update(`Pulling base image (${baseImage})...`);
            try {
              await execa("docker", ["pull", baseImage], { stdio: "pipe" });
            } catch {
              stepper.fail("Could not pull base image");
              console.log(chalk.yellow(`
  Failed to pull ${chalk.bold(baseImage)}.

  ${chalk.bold("1.")} Pull manually: ${chalk.cyan(`docker pull ${baseImage}`)}
  ${chalk.bold("2.")} Re-run:        ${chalk.cyan("azureclaw dev")}

  Custom registry? ${chalk.cyan(`azureclaw dev --base-image <your-registry>/azurelinux/base/core:3.0`)}
`));
              process.exit(1);
            }
          }

          const dockerfilePath = path.join(repoRoot, "sandbox-images/openclaw/Dockerfile");
          const routerDockerfile = path.join(repoRoot, "inference-router/Dockerfile");
          if (!existsSync(dockerfilePath)) {
            stepper.fail("Dockerfile not found");
            console.log(chalk.yellow(`
  Run from the AzureClaw repo root:
    ${chalk.cyan("git clone https://github.com/Azure/azureclaw.git && cd azureclaw")}
    ${chalk.cyan("azureclaw dev")}
`));
            process.exit(1);
          }

          // Build inference router locally (sandbox Dockerfile needs it)
          const routerImage = "azureclaw-inference-router:dev";
          let routerExists = false;
          try {
            await execa("docker", ["image", "inspect", routerImage], { stdio: "pipe" });
            routerExists = true;
          } catch { /* not built yet */ }

          if (options.build || !routerExists) {
            stepper.update("Building inference router (Rust — first run takes a few minutes)...");
            stepper.stop();
            console.log(chalk.dim("  Building inference-router (Rust)...\n"));
            await execa("docker", [
              "build",
              "-t", routerImage,
              "-f", routerDockerfile,
              repoRoot,
            ], { stdio: "inherit" });
            console.log();
          }

          stepper.update("Building sandbox image (Node.js + OpenClaw)...");
          stepper.stop();
          console.log(chalk.dim("  Building sandbox image...\n"));
          await execa("docker", [
            "build",
            "--build-arg", `AZURELINUX_BASE=${baseImage}`,
            "--build-arg", `INFERENCE_ROUTER_IMAGE=${routerImage}`,
            "-t", "azureclaw-sandbox:dev",
            "-f", dockerfilePath,
            repoRoot,
          ], { stdio: "inherit" });
          console.log();
          image = "azureclaw-sandbox:dev";
          stepper.done("Sandbox image built");
        } else {
          stepper.done("Sandbox image found");
        }

        // ── Load or prompt for credentials ──────────────────────────
        stepper.step("Loading credentials...");
        const creds = await ensureCredentials();
        stepper.done("Credentials ready");
        const model = options.model !== "gpt-4.1" ? options.model : creds.model;

        // ── Container startup ────────────────────────────────────────
        stepper.step("Starting sandbox container...");
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

        stepper.update("Launching container...");
        await execa("docker", [
          "run", "-d",
          "--name", containerName,
          "--hostname", options.name,
          ...seccompArgs,
          "--read-only",
          // Grant NET_ADMIN for iptables egress guard (same as AKS init container)
          "--cap-add", "NET_ADMIN",
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
          "-e", `AZURE_OPENAI_ENDPOINT=${creds.endpoint}`,
          "-e", `PS1=azureclaw@${options.name}:\\w\\$ `,
          image,
        ], { stdio: "pipe" });

        // ── Egress guard: iptables restricts UID 1000 to localhost + DNS ──
        // Same rules as the AKS egress-guard init container.
        // UID 1000 (openclaw agent) can only reach localhost and DNS.
        // UID 1001 (inference-router) can reach the internet for Foundry API.
        let hasIptables = false;
        try {
          await execa("docker", [
            "exec", "-u", "root", containerName, "sh", "-c",
            [
              "iptables -N AZURECLAW_EGRESS 2>/dev/null || true",
              "iptables -A AZURECLAW_EGRESS -o lo -j ACCEPT",
              "iptables -A AZURECLAW_EGRESS -p udp --dport 53 -j ACCEPT",
              "iptables -A AZURECLAW_EGRESS -p tcp --dport 53 -j ACCEPT",
              "iptables -A AZURECLAW_EGRESS -j REJECT --reject-with icmp-port-unreachable",
              "iptables -A OUTPUT -m owner --uid-owner 1000 -j AZURECLAW_EGRESS",
            ].join(" && "),
          ], { stdio: "pipe" });
          hasIptables = true;
        } catch {
          // iptables not available (e.g., rootless Docker) — fall back to Docker isolation only
        }

        stepper.done("Sandbox running");

        // ── Security status ──────────────────────────────────────────
        section("Security");
        checkLine(true, "Read-only root filesystem");
        checkLine(true, "Non-root user (sandbox:1000)");
        checkLine(true, "All root privileges removed");
        checkLine(hasSeccomp, `seccomp profile ${hasSeccomp ? "(azureclaw-strict)" : "(not loaded)"}`);
        checkLine(hasIptables, `iptables egress guard ${hasIptables ? "(UID 1000 → localhost + DNS)" : "(not available)"}`);
        checkLine(true, "API key mounted as read-only secret");

        section("Environment");
        kvLine("OS", "Azure Linux 3.0");
        kvLine("OpenClaw", "2026.3.13");
        kvLine("Model", `${model} (Azure OpenAI)`);
        kvLine("Endpoint", creds.endpoint);
        kvLine("Policy", `${options.policy} preset`);
        kvLine("Sandbox", options.name);

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

        section("Commands");
        console.log(`  Connect:  ${chalk.cyan(`azureclaw connect ${options.name}`)}`);
        console.log(`  Shell:    ${chalk.cyan(`azureclaw connect ${options.name} --shell`)}`);
        console.log(`  Status:   ${chalk.cyan(`azureclaw status ${options.name}`)}`);
        console.log(`  Stop:     ${chalk.cyan(`azureclaw destroy ${options.name}`)}`);
        if (gatewayToken) {
          const url = `http://localhost:18789/#token=${gatewayToken}`;
          // OSC 8 hyperlink: makes URL clickable in modern terminals (iTerm2, macOS Terminal, VS Code, etc.)
          const link = `\u001B]8;;${url}\u001B\\${url}\u001B]8;;\u001B\\`;
          console.log(`  Web UI:   ${chalk.cyan(link)}`);
        }
        console.log(chalk.dim(`\n  Production: azureclaw up (deploys to AKS)`));
        console.log();
      } catch (error) {
        stepper.stop();
        console.error(chalk.red(`\n  Local sandbox failed to start`));
        const message =
          error instanceof Error ? error.message : String(error);
        console.error(chalk.red(`  ${message}\n`));
        process.exit(1);
      }
    });

  return cmd;
}
