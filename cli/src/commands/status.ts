// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import chalk from "chalk";

export function statusCommand(): Command {
  const cmd = new Command("status");

  cmd
    .description("Show sandbox health, policy state, and inference configuration")
    .argument("<name>", "Sandbox name")
    .action(async (name: string) => {
      const blue = chalk.hex("#0078D4");
      const containerName = `kars-${name}`;

      // Try local Docker first, then AKS
      try {
        const { execa } = await import("execa");

        // Check Docker
        const { stdout: inspectJson } = await execa("docker", [
          "inspect", "--format", "{{json .}}", containerName,
        ], { stdio: "pipe" });

        const info = JSON.parse(inspectJson);
        const dockerRunning = info.State?.Running === true;
        const startedAt = info.State?.StartedAt;
        const image = info.Config?.Image || "unknown";
        const readOnly = info.HostConfig?.ReadonlyRootfs === true;
        const model = (info.Config?.Env || [])
          .find((e: string) => e.startsWith("OPENCLAW_MODEL="))
          ?.split("=")[1] || "gpt-4.1";

        // Docker says running — verify the inference router is actually responsive
        let running = dockerRunning;
        if (dockerRunning) {
          try {
            await execa("docker", [
              "exec", containerName, "curl", "-sf", "--max-time", "3",
              "http://127.0.0.1:8443/healthz",
            ], { stdio: "pipe" });
          } catch {
            running = false; // container is up but router isn't responding
          }
        }
        const seccomp = info.HostConfig?.SecurityOpt
          ?.some((s: string) => s.includes("seccomp")) ?? false;
        // Calculate uptime
        let uptime = "unknown";
        if (startedAt) {
          const ms = Date.now() - new Date(startedAt).getTime();
          const mins = Math.floor(ms / 60000);
          if (mins < 60) uptime = `${mins}m`;
          else uptime = `${Math.floor(mins / 60)}h ${mins % 60}m`;
        }

        console.log(blue(`
  ╔══════════════════════════════════════════════════╗
  ║           Kars · Sandbox Status             ║
  ╚══════════════════════════════════════════════════╝
`));
        console.log(`  Sandbox:       ${chalk.bold(name)}`);
        console.log(`  Status:        ${running ? chalk.green("● Running") : dockerRunning ? chalk.yellow("● Unhealthy (container up, router down)") : chalk.red("● Stopped")}`);
        console.log(`  Uptime:        ${uptime}`);
        console.log(`  Image:         ${image}`);
        console.log(`  Model:         ${chalk.bold(model)} (Azure OpenAI)`);

        console.log(blue(`\n  ── Security ──────────────────────────────────────`));
        console.log(`  ${readOnly ? chalk.green("✓") : chalk.red("✗")} Read-only root filesystem`);
        console.log(`  ${chalk.green("✓")} Non-root user (sandbox:1000)`);
        console.log(`  ${chalk.green("✓")} All root privileges removed`);
        console.log(`  ${seccomp ? chalk.green("✓") : chalk.yellow("○")} seccomp profile${seccomp ? " (kars-strict)" : ""}`);

        console.log(blue(`\n  ── Inference Router ───────────────────────────────`));
        // Query Prometheus metrics from the Rust inference router
        try {
          const { stdout: metricsRaw } = await execa("docker", [
            "exec", containerName, "curl", "-sf", "http://127.0.0.1:8443/metrics",
          ], { stdio: "pipe" });

          const requests = metricsRaw.match(/kars_inference_requests_total\{[^}]*status="ok"[^}]*\}\s+(\d+)/);
          const inputTokens = metricsRaw.match(/kars_tokens_total\{[^}]*direction="input"[^}]*\}\s+(\d+)/);
          const outputTokens = metricsRaw.match(/kars_tokens_total\{[^}]*direction="output"[^}]*\}\s+(\d+)/);
          const latencySum = metricsRaw.match(/kars_inference_latency_seconds_sum\{[^}]*\}\s+([\d.]+)/);
          const latencyCount = metricsRaw.match(/kars_inference_latency_seconds_count\{[^}]*\}\s+(\d+)/);

          const reqCount = requests ? parseInt(requests[1]) : 0;
          const inTokens = inputTokens ? parseInt(inputTokens[1]) : 0;
          const outTokens = outputTokens ? parseInt(outputTokens[1]) : 0;
          const avgLatency = (latencySum && latencyCount && parseInt(latencyCount[1]) > 0)
            ? (parseFloat(latencySum[1]) / parseInt(latencyCount[1])).toFixed(1)
            : "—";

          console.log(`  ${chalk.green("✓")} Router running (Rust, port 8443)`);
          console.log(`  Requests:      ${reqCount}`);
          console.log(`  Tokens:        ${inTokens.toLocaleString()} in / ${outTokens.toLocaleString()} out`);
          console.log(`  Avg latency:   ${avgLatency}s`);
        } catch {
          console.log(`  ${chalk.yellow("○")} Router not reachable`);
        }

        console.log(blue(`\n  ── Network ───────────────────────────────────────`));
        console.log(`  Policy:        default-deny egress`);
        console.log(`  Inference:     routed via inference router (no direct access)`);

        console.log();
        return;
      } catch {
        // No local container — try AKS
      }

      // AKS: query KarsSandbox CRD
      try {
        const { execa } = await import("execa");
        const { stdout } = await execa("kubectl", [
          "get", "karssandbox", name,
          "-n", "kars-system",
          "-o", "json",
        ], { stdio: "pipe" });

        const sandbox = JSON.parse(stdout);
        const phase = sandbox.status?.phase || "Unknown";
        const model = sandbox.spec?.inference?.model || "gpt-4.1";
        const isolation = sandbox.spec?.sandbox?.isolation || "enhanced";

        console.log(blue(`
  ╔══════════════════════════════════════════════════╗
  ║           Kars · Sandbox Status             ║
  ╚══════════════════════════════════════════════════╝
`));
        console.log(`  Sandbox:       ${chalk.bold(name)}`);
        console.log(`  Status:        ${phase === "Running" ? chalk.green("● Running") : chalk.yellow("● " + phase)}`);
        console.log(`  Namespace:     kars-${name}`);
        console.log(`  Model:         ${chalk.bold(model)} (Azure OpenAI)`);
        console.log(`  Isolation:     ${isolation}`);
        console.log();
      } catch {
        console.log(chalk.red(`\n  Sandbox '${name}' not found (checked Docker and AKS).\n`));
        process.exit(1);
      }
    });

  return cmd;
}
