// Phase 2 / S15.b: `azureclaw mesh promote` subcommand body extracted
// from mesh.ts. Attaches as a subcommand of an existing Commander command.

import chalk from "chalk";
import { Command } from "commander";
import * as fs from "node:fs";
import * as http from "node:http";
import * as os from "node:os";
import * as path from "node:path";
import { spawn } from "node:child_process";
import { execa } from "execa";
import { banner, section, kvLine, checkLine } from "../../stepper.js";
import { loadContext, saveContext } from "../../config.js";
import {
  checkRegistryHealth,
  checkRelayHealth,
  findDuplicateListeners,
  killProcessesOnPorts,
  killStaleListeners,
} from "./health.js";

export function attachPromoteSubcommand(cmd: Command): void {
  cmd
    .command("promote")
    .description("Promote the AKS cluster registry to a public global endpoint")
    .option("--allow-ip <cidr>", "Restrict access to this IP/CIDR (LoadBalancer mode)")
    .option("--port-forward", "Use kubectl port-forward instead of LoadBalancer (recommended for Cilium clusters)")
    .option("--registry-port <port>", "Local port for registry (port-forward mode)", "18080")
    .option("--relay-port <port>", "Local port for relay (port-forward mode)", "18765")
    .action(async (opts: { allowIp?: string; portForward?: boolean; registryPort?: string; relayPort?: string }) => {
      banner("AzureClaw · Mesh Promote", "Promote Registry to Global");

      // Load deployment context
      const ctx = loadContext();
      if (!ctx?.aksCluster || !ctx?.resourceGroup) {
        console.error(chalk.red("  ✘ No deployment context found."));
        console.error(chalk.dim("    Run azureclaw up first to deploy an AKS cluster."));
        process.exit(1);
      }

      if (ctx.registryMode === "global" && ctx.globalRegistryUrl) {
        // Already promoted — check health and reconnect if needed
        const isPortForward = ctx.promoteMode === "port-forward";
        const regPort = parseInt(opts.registryPort ?? "18080", 10);
        const relayPort = parseInt(opts.relayPort ?? "18765", 10);
        const pidFile = path.join(os.homedir(), ".azureclaw", "port-forward-pids.json");

        console.log(chalk.dim("  Registry was previously promoted — checking health...\n"));

        // ── Health check: registry + relay ──
        const registryHealthy = await checkRegistryHealth(regPort);
        const relayHealthy = await checkRelayHealth(relayPort);

        // Check for duplicate port-forwards (common source of flaky connections)
        const duplicates = await findDuplicateListeners([regPort, relayPort]);
        if (duplicates.length > 0) {
          console.log();
          console.log(chalk.yellow("  ⚠ Duplicate listeners detected:"));
          for (const d of duplicates) {
            console.log(chalk.dim(`    Port ${d.port}: ${d.count} processes (PIDs: ${d.pids.join(", ")})`));
          }
          console.log(chalk.dim("    Will kill duplicates and reconnect...\n"));
          // Fall through to reconnect logic below
        } else if (registryHealthy && relayHealthy) {
          console.log();
          console.log(chalk.green("  ✓ ") + chalk.bold("Mesh is healthy — all tunnels active."));
          kvLine("Registry", chalk.cyan(ctx.globalRegistryUrl));
          kvLine("Relay", chalk.cyan(ctx.globalRelayUrl ?? "—"));
          console.log();
          return;
        }

        // ── Reconnect: kill stale port-forwards, restart ──
        if (isPortForward) {
          section("Reconnecting Port-Forwards");

          // Kill stale PIDs
          try {
            const savedPids = JSON.parse(fs.readFileSync(pidFile, "utf-8")) as Record<string, number>;
            for (const [label, pid] of Object.entries(savedPids)) {
              try {
                process.kill(pid, "SIGTERM");
                console.log(chalk.dim(`  · Stopped stale ${label} tunnel (PID ${pid})`));
              } catch { /* already dead */ }
            }
          } catch { /* no PID file */ }
          await new Promise(r => setTimeout(r, 1000));

          // Kill anything still listening on the ports (not connected clients)
          for (const port of [regPort, relayPort]) {
            try {
              const { stdout } = await execa("lsof", ["-ti", `:${port}`, "-sTCP:LISTEN"], { stdio: "pipe", reject: false });
              const pidsOnPort = stdout.trim().split("\n").filter(Boolean);
              for (const p of pidsOnPort) {
                try { process.kill(parseInt(p, 10), "SIGKILL"); } catch { /* ignore */ }
              }
            } catch { /* no process on port */ }
          }
          await new Promise(r => setTimeout(r, 1000));

          // Start fresh tunnels
          const tunnels = [
            { svc: "svc/agentmesh-registry", localPort: regPort, remotePort: 8080, label: "Registry" },
            { svc: "svc/agentmesh-relay", localPort: relayPort, remotePort: 8765, label: "Relay" },
          ];

          const pids: Record<string, number> = {};
          for (const t of tunnels) {
            const logDir = path.join(os.homedir(), ".azureclaw", "logs");
            fs.mkdirSync(logDir, { recursive: true });
            const outFd = fs.openSync(path.join(logDir, `pf-${t.label.toLowerCase()}.log`), "w");

            const child = spawn("kubectl", [
              "port-forward", t.svc, `${t.localPort}:${t.remotePort}`,
              "-n", "agentmesh", "--address", "0.0.0.0",
            ], { stdio: ["ignore", outFd, outFd], detached: true });

            const logPath = path.join(logDir, `pf-${t.label.toLowerCase()}.log`);
            let ready = false;
            for (let attempt = 0; attempt < 30; attempt++) {
              await new Promise(r => setTimeout(r, 500));
              try {
                const content = fs.readFileSync(logPath, "utf-8");
                if (content.includes("Forwarding from")) { ready = true; break; }
              } catch { /* file not written yet */ }
            }

            child.unref();
            fs.closeSync(outFd);

            if (ready) {
              pids[t.label] = child.pid!;
              checkLine(true, `${t.label}: localhost:${t.localPort} → ${t.svc}:${t.remotePort} (PID ${child.pid})`);
            } else {
              console.error(chalk.red(`  ✘ Port-forward for ${t.label} failed to start.`));
              process.exit(1);
            }
          }

          fs.mkdirSync(path.dirname(pidFile), { recursive: true });
          fs.writeFileSync(pidFile, JSON.stringify(pids, null, 2));

          // Kill any stale listeners that aren't our spawned PIDs
          await killStaleListeners([
            { port: regPort, pid: pids.Registry },
            { port: relayPort, pid: pids.Relay },
          ]);

          // Final health check
          section("Connectivity Check");
          await checkRegistryHealth(regPort);
          await checkRelayHealth(relayPort);

          // Update context (URLs may have changed if custom ports)
          ctx.globalRegistryUrl = `http://localhost:${regPort}`;
          ctx.globalRelayUrl = `ws://localhost:${relayPort}`;
          saveContext(ctx);

          section("Global Endpoints");
          kvLine("Registry", chalk.cyan(ctx.globalRegistryUrl));
          kvLine("Relay", chalk.cyan(ctx.globalRelayUrl));

          console.log();
          console.log(chalk.green("  ✓ ") + chalk.bold("Port-forwards reconnected."));
          console.log(chalk.dim(`    PIDs saved to ${pidFile}`));
          console.log();
        } else {
          // LoadBalancer mode — just report the broken state
          console.log(chalk.yellow("\n  ⚠ Endpoints are not healthy. Run azureclaw mesh demote and re-promote."));
        }
        return;
      }

      section("Cluster");
      kvLine("AKS", ctx.aksCluster);
      kvLine("Resource Group", ctx.resourceGroup);
      kvLine("ACR", ctx.acrLoginServer ?? "—");

      // Verify agentmesh namespace exists
      section("AgentMesh");
      try {
        await execa("kubectl", ["get", "namespace", "agentmesh"], { stdio: "pipe" });
        checkLine(true, "agentmesh namespace exists");
      } catch {
        console.error(chalk.red("  ✘ agentmesh namespace not found."));
        console.error(chalk.dim("    Deploy an agent first: azureclaw up <name> --model <model>"));
        process.exit(1);
      }

      // Verify pods are running
      for (const app of ["agentmesh-registry", "agentmesh-relay"]) {
        try {
          await execa("kubectl", [
            "get", "pod", "-n", "agentmesh", "-l", `app=${app}`,
            "--field-selector", "status.phase=Running", "-o", "name",
          ], { stdio: "pipe" });
          checkLine(true, `${app.replace("agentmesh-", "")} pod running`);
        } catch {
          console.error(chalk.red(`  ✘ ${app} pod not running.`));
          process.exit(1);
        }
      }

      // ── Port-forward mode ──────────────────────────────────────────────
      if (opts.portForward) {
        const regPort = parseInt(opts.registryPort ?? "18080", 10);
        const relayPort = parseInt(opts.relayPort ?? "18765", 10);

        section("Port-Forward Tunnels");
        console.log(chalk.dim("  Tunnelling through kubectl (bypasses Azure LB/Cilium)"));

        // Kill any existing processes on these ports to prevent duplicates
        await killProcessesOnPorts([regPort, relayPort]);

        const tunnels = [
          { svc: "svc/agentmesh-registry", localPort: regPort, remotePort: 8080, label: "Registry" },
          { svc: "svc/agentmesh-relay", localPort: relayPort, remotePort: 8765, label: "Relay" },
        ];

        const pidFile = path.join(os.homedir(), ".azureclaw", "port-forward-pids.json");
        const pids: Record<string, number> = {};

        for (const t of tunnels) {
          // Open log files so kubectl port-forward has somewhere to write
          const logDir = path.join(os.homedir(), ".azureclaw", "logs");
          fs.mkdirSync(logDir, { recursive: true });
          const outFd = fs.openSync(path.join(logDir, `pf-${t.label.toLowerCase()}.log`), "w");

          const child = spawn("kubectl", [
            "port-forward", t.svc, `${t.localPort}:${t.remotePort}`,
            "-n", "agentmesh", "--address", "0.0.0.0",
          ], { stdio: ["ignore", outFd, outFd], detached: true });

          // Wait for port-forward to be ready by polling the log file
          const logPath = path.join(logDir, `pf-${t.label.toLowerCase()}.log`);
          let ready = false;
          for (let attempt = 0; attempt < 30; attempt++) {
            await new Promise(r => setTimeout(r, 500));
            try {
              const content = fs.readFileSync(logPath, "utf-8");
              if (content.includes("Forwarding from")) {
                ready = true;
                break;
              }
            } catch { /* file not written yet */ }
          }

          child.unref();
          fs.closeSync(outFd);

          if (ready) {
            pids[t.label] = child.pid!;
            checkLine(true, `${t.label}: localhost:${t.localPort} → ${t.svc}:${t.remotePort} (PID ${child.pid})`);
          } else {
            console.error(chalk.red(`  ✘ Port-forward for ${t.label} failed to start.`));
            process.exit(1);
          }
        }

        // Save PIDs for demote cleanup
        fs.mkdirSync(path.dirname(pidFile), { recursive: true });
        fs.writeFileSync(pidFile, JSON.stringify(pids, null, 2));

        // Kill any stale listeners that aren't our spawned PIDs
        await killStaleListeners([
          { port: regPort, pid: pids.Registry },
          { port: relayPort, pid: pids.Relay },
        ]);

        // Verify connectivity
        section("Connectivity Check");
        const regHealthy = await checkRegistryHealth(regPort);
        const relayOk = await checkRelayHealth(relayPort);

        const globalRegistryUrl = `http://localhost:${regPort}`;
        const globalRelayUrl = `ws://localhost:${relayPort}`;

        ctx.registryMode = "global";
        ctx.globalRegistryUrl = globalRegistryUrl;
        ctx.globalRelayUrl = globalRelayUrl;
        ctx.promoteMode = "port-forward";
        saveContext(ctx);

        section("Global Endpoints");
        kvLine("Registry", chalk.cyan(globalRegistryUrl));
        kvLine("Relay", chalk.cyan(globalRelayUrl));

        console.log();
        console.log(chalk.green("  ✓ ") + chalk.bold("Registry promoted to global (port-forward)."));
        console.log(chalk.dim("    Tunnels are running in the background."));
        console.log(chalk.dim(`    PIDs saved to ${pidFile}`));
        console.log(chalk.dim(`\n    Test:  curl ${globalRegistryUrl}/v1/health`));
        console.log(chalk.dim(`    Then:  azureclaw dev --global-registry ${globalRegistryUrl}`));
        console.log(chalk.dim(`    Stop:  azureclaw mesh demote`));
        console.log();
        return;
      }

      // ── LoadBalancer mode (original) ───────────────────────────────────
      section("Access Control");
      let allowCidr: string;

      if (opts.allowIp) {
        allowCidr = opts.allowIp.includes("/") ? opts.allowIp : `${opts.allowIp}/32`;
        kvLine("Allow IP", allowCidr + " (from --allow-ip)");
      } else {
        let detectedIp = "";
        try {
          const resp = await fetch("https://ifconfig.me/ip", { signal: AbortSignal.timeout(5000) });
          if (resp.ok) {
            const ip = (await resp.text()).trim();
            if (/^\d{1,3}(\.\d{1,3}){3}$/.test(ip)) {
              detectedIp = ip;
            }
          }
        } catch { /* fall through */ }

        if (!detectedIp) {
          console.error(chalk.red("  ✘ Could not detect public IP."));
          console.error(chalk.dim("    Use --allow-ip <your-ip> to specify manually."));
          process.exit(1);
        }
        allowCidr = `${detectedIp}/32`;
        kvLine("Allow IP", allowCidr + " (auto-detected)");
      }

      section("LoadBalancer Services");

      const services = [
        { name: "agentmesh-registry", port: 8080, label: "Registry" },
        { name: "agentmesh-relay", port: 8765, label: "Relay" },
      ];

      for (const svc of services) {
        console.log(chalk.dim(`  Patching ${svc.name} → LoadBalancer...`));
        const patch = {
          spec: {
            type: "LoadBalancer",
            loadBalancerSourceRanges: [allowCidr],
          },
        };
        try {
          await execa("kubectl", [
            "patch", "svc", svc.name, "-n", "agentmesh",
            "--type", "merge",
            "-p", JSON.stringify(patch),
          ], { stdio: "pipe" });
          checkLine(true, `${svc.label} → LoadBalancer (restricted to ${allowCidr})`);
        } catch (e: any) {
          console.error(chalk.red(`  ✘ Failed to patch ${svc.name}: ${e.message}`));
          process.exit(1);
        }
      }

      section("Waiting for External IPs");
      const externalIps: Record<string, string> = {};

      for (const svc of services) {
        console.log(chalk.dim(`  Waiting for ${svc.label} IP...`));
        let ip = "";
        for (let i = 0; i < 30; i++) {
          try {
            const { stdout } = await execa("kubectl", [
              "get", "svc", svc.name, "-n", "agentmesh",
              "-o", "jsonpath={.status.loadBalancer.ingress[0].ip}",
            ], { stdio: "pipe" });
            if (stdout.trim() && /^\d/.test(stdout.trim())) {
              ip = stdout.trim();
              break;
            }
          } catch { /* retry */ }
          await new Promise(r => setTimeout(r, 5000));
        }

        if (!ip) {
          console.error(chalk.red(`  ✘ Timed out waiting for ${svc.label} external IP.`));
          process.exit(1);
        }
        externalIps[svc.name] = ip;
        checkLine(true, `${svc.label}: ${ip}:${svc.port}`);
      }

      const registryIp = externalIps["agentmesh-registry"];
      const relayIp = externalIps["agentmesh-relay"];
      const registrySslip = registryIp.replace(/\./g, "-") + ".sslip.io";
      const relaySslip = relayIp.replace(/\./g, "-") + ".sslip.io";

      const globalRegistryUrl = `http://${registrySslip}:8080`;
      const globalRelayUrl = `ws://${relaySslip}:8765`;

      ctx.registryMode = "global";
      ctx.globalRegistryUrl = globalRegistryUrl;
      ctx.globalRelayUrl = globalRelayUrl;
      ctx.promoteMode = "loadbalancer";
      saveContext(ctx);

      section("Global Endpoints");
      kvLine("Registry", chalk.cyan(globalRegistryUrl));
      kvLine("Relay", chalk.cyan(globalRelayUrl));

      console.log();
      console.log(chalk.green("  ✓ ") + chalk.bold("Registry promoted to global."));
      console.log(chalk.dim("    Using sslip.io for DNS (auto-resolved, no setup needed)."));
      console.log(chalk.dim("    HTTP only — secured by LoadBalancer IP allowlist."));
      console.log(chalk.dim(`\n    Test:  curl ${globalRegistryUrl}/v1/health`));
      console.log(chalk.dim(`    Then:  azureclaw dev --global-registry ${globalRegistryUrl}`));
      console.log();
    });
}
