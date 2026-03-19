import { Command } from "commander";
import chalk from "chalk";

export function connectCommand(): Command {
  const cmd = new Command("connect");

  cmd
    .description("Connect to a sandbox — shell, TUI, or WebUI")
    .argument("<name>", "Sandbox name")
    .option("--shell", "Drop to bash shell instead of OpenClaw", false)
    .option("--web", "Open WebUI via port-forward (default for AKS)", false)
    .option("--port <port>", "Local port for WebUI", "18789")
    .action(async (name: string, options: { shell: boolean; web: boolean; port: string }) => {
      const { execa } = await import("execa");
      const containerName = `azureclaw-${name}`;
      const namespace = `azureclaw-${name}`;
      const localPort = options.port;

      // Try local Docker first (azureclaw dev mode)
      if (!options.web) {
        try {
          const { stdout } = await execa("docker", [
            "inspect", "--format", "{{.State.Running}}", containerName,
          ], { stdio: "pipe" });

          if (stdout.trim() === "true") {
            console.log(chalk.hex("#0078D4")(`\n  Connected to ${chalk.bold(name)}. OpenClaw is ready.\n`));
            console.log(chalk.dim(`  Chat:    openclaw tui`));
            console.log(chalk.dim(`  Message: openclaw agent --agent main --local -m "hello" --session-id test`));
            console.log(chalk.dim(`  Exit:    type "exit"\n`));
            await execa("docker", [
              "exec", "-it", containerName, "/bin/bash", "--login",
            ], { stdio: "inherit" });
            return;
          }
        } catch {
          // Not a local container — try AKS
        }
      }

      // AKS mode: check if pod exists
      try {
        await execa("kubectl", [
          "get", "deploy", name, "-n", namespace, "--no-headers",
        ], { stdio: "pipe" });
      } catch {
        console.log(chalk.red(`\n  Sandbox '${name}' not found.`));
        console.log(chalk.dim(`  Run: azureclaw up --name ${name}\n`));
        return;
      }

      // --web or --shell?
      if (options.web || !options.shell) {
        // WebUI mode: extract token, port-forward, print link
        console.log(chalk.hex("#0078D4")(`\n  Connecting to ${chalk.bold(name)} WebUI...\n`));

        // Check if this is a Kata (confidential) sandbox — port-forward doesn't work with Kata VMs
        let isKata = false;
        try {
          const { stdout: rc } = await execa("kubectl", [
            "get", "pod", "-n", namespace, "-l", `azureclaw.azure.com/sandbox=${name}`,
            "-o", "jsonpath={.items[0].spec.runtimeClassName}",
          ], { stdio: "pipe" });
          isKata = rc.trim().includes("kata");
        } catch {}

        // Extract gateway token
        let gatewayToken = "";
        try {
          const { stdout: bashrc } = await execa("kubectl", [
            "exec", "-n", namespace, `deploy/${name}`,
            "-c", "openclaw", "--",
            "cat", "/sandbox/.bashrc",
          ], { stdio: "pipe" });
          const match = bashrc.match(/OPENCLAW_GATEWAY_TOKEN="([^"]+)"/);
          if (match) gatewayToken = match[1];
        } catch {
          console.log(chalk.yellow("  Could not extract gateway token."));
        }

        if (!gatewayToken) {
          console.log(chalk.red("  Gateway token not found. Is the sandbox running?\n"));
          return;
        }

        if (isKata) {
          // Kata VMs don't support kubectl port-forward — use shell mode instead
          console.log(chalk.yellow("  Note: Kata VM pods don't support port-forward (known limitation)."));
          console.log(chalk.yellow("  The WebUI is accessible from inside the cluster only.\n"));
          console.log(chalk.dim(`  Gateway token: ${gatewayToken}`));
          console.log(chalk.dim(`  To access the WebUI, use an enhanced (non-Kata) sandbox:\n`));
          console.log(`  ${chalk.cyan(`azureclaw up --skip-infra --isolation enhanced --name ${name}-web`)}`);
          console.log();
          console.log(chalk.dim("  Falling back to shell mode...\n"));
          await execa("kubectl", [
            "exec", "-it", "-n", namespace,
            `deploy/${name}`, "-c", "openclaw",
            "--", "/bin/bash", "--login",
          ], { stdio: "inherit" });
          return;
        }

        // Start port-forward
        console.log(chalk.dim(`  Starting port-forward on localhost:${localPort}...`));
        const pf = execa("kubectl", [
          "port-forward", "-n", namespace,
          `deploy/${name}`, `${localPort}:18789`,
        ], { stdio: "pipe" });

        // Wait for port-forward to be ready
        await new Promise(r => setTimeout(r, 2000));

        const url = `http://localhost:${localPort}/#token=${gatewayToken}`;
        console.log();
        console.log(`  ${chalk.green("→")} ${chalk.cyan.underline(url)}`);
        console.log();
        console.log(chalk.dim(`  Port-forward active. Press Ctrl+C to disconnect.\n`));

        // Keep alive until Ctrl+C
        try {
          await pf;
        } catch {
          // User pressed Ctrl+C
          console.log(chalk.dim("\n  Disconnected.\n"));
        }
      } else {
        // Shell mode
        console.log(chalk.hex("#0078D4")(`\n  Connected to ${chalk.bold(name)}. OpenClaw is ready.\n`));
        console.log(chalk.dim(`  Chat:    openclaw tui`));
        console.log(chalk.dim(`  Exit:    type "exit"\n`));
        await execa("kubectl", [
          "exec", "-it", "-n", namespace,
          `deploy/${name}`, "-c", "openclaw",
          "--", "/bin/bash", "--login",
        ], { stdio: "inherit" });
      }
    });

  return cmd;
}
