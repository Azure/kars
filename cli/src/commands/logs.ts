// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import chalk from "chalk";

export function logsCommand(): Command {
  const cmd = new Command("logs");

  cmd
    .description("Stream agent and platform logs")
    .argument("<name>", "Sandbox name")
    .option("-f, --follow", "Follow log output", false)
    .option("--tail <lines>", "Number of lines to show", "100")
    .option("--service <svc>", "Service to show logs for (router, gateway, openclaw, node-host, all)", "all")
    .action(async (name: string, options) => {
      const { execa } = await import("execa");
      const containerName = `azureclaw-${name}`;

      // Try local Docker first
      let isDocker = false;
      try {
        const { stdout } = await execa("docker", [
          "inspect", "--format", "{{.State.Running}}", containerName,
        ], { stdio: "pipe" });
        if (stdout.trim() === "true") isDocker = true;
      } catch {
        // Not a Docker container
      }

      if (isDocker) {
        const logFiles: Record<string, string> = {
          "router": "/tmp/inference-router.log",
          "gateway": "/tmp/gateway.log",
          "openclaw": "/tmp/openclaw/*.log",
          "node-host": "/tmp/node-host.log",
        };

        const services = options.service === "all"
          ? Object.keys(logFiles)
          : [options.service];

        for (const svc of services) {
          const logPath = logFiles[svc];
          if (!logPath) {
            console.log(chalk.yellow(`  Unknown service: ${svc}`));
            console.log(chalk.dim(`  Available: ${Object.keys(logFiles).join(", ")}, all\n`));
            continue;
          }

          console.log(chalk.hex("#0078D4")(`\n  ── ${svc} ─────────────────────────────────────`));
          try {
            const tailArg = options.follow ? "-f" : `-n ${options.tail}`;
            const shellCmd = `tail ${tailArg} ${logPath} 2>/dev/null || echo '  (no log file yet)'`;
            if (options.follow && services.length === 1) {
              // Follow mode: stream to stdout
              await execa("docker", [
                "exec", containerName, "sh", "-c", shellCmd,
              ], { stdio: "inherit" });
            } else {
              const { stdout } = await execa("docker", [
                "exec", containerName, "sh", "-c", shellCmd,
              ], { stdio: "pipe" });
              console.log(stdout);
            }
          } catch {
            console.log(chalk.dim(`  (no logs available)`));
          }
        }
        console.log();
        return;
      }

      // Fall back to Kubernetes
      const namespace = `azureclaw-${name}`;
      const args = [
        "logs",
        "-n",
        namespace,
        "-l",
        `azureclaw.azure.com/sandbox=${name}`,
        "--tail",
        options.tail,
      ];

      if (options.follow) {
        args.push("-f");
      }

      try {
        await execa("kubectl", args, { stdio: "inherit" });
      } catch {
        console.log(chalk.red(`\n  No sandbox '${name}' found (checked Docker and AKS).\n`));
      }
    });

  return cmd;
}
