// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import chalk from "chalk";
import { agentContainerName, runtimeKindFromCr, type RuntimeKind } from "../runtime.js";

export function connectCommand(): Command {
  const cmd = new Command("connect");

  cmd
    .description("Connect to a sandbox — shell, TUI, or WebUI")
    .argument("<name>", "Sandbox name")
    .option("--shell", "Drop to bash shell instead of OpenClaw", false)
    .option("--web", "Open WebUI via port-forward (default for AKS)", false)
    .option("--local", "Connect to local Docker sandbox (skip AKS)", false)
    .option("--cloud", "Connect to AKS cloud sandbox (skip Docker)", false)
    .option("--port <port>", "Local port for WebUI", "18789")
    .action(async (name: string, options: { shell: boolean; web: boolean; local: boolean; cloud: boolean; port: string }) => {
      const { execa } = await import("execa");
      const containerName = `azureclaw-${name}`;
      const namespace = `azureclaw-${name}`;
      const localPort = options.port;

      // Detect where the agent exists
      let localRunning = false;
      let localExists = false;
      let aksExists = false;

      if (!options.cloud) {
        try {
          const { stdout } = await execa("docker", [
            "inspect", "--format", "{{.State.Running}}", containerName,
          ], { stdio: "pipe" });
          localExists = true;
          localRunning = stdout.trim() === "true";
        } catch { /* no local container */ }
      }

      if (!options.local) {
        try {
          await execa("kubectl", [
            "get", "deploy", name, "-n", namespace, "--no-headers",
          ], { stdio: "pipe" });
          aksExists = true;
        } catch { /* no AKS deployment */ }
      }

      // S10.A5: resolve container name from the live ClawSandbox CR
      // so non-OpenClaw runtimes (OpenAIAgents, MAF, BYO) hit the
      // generic `agent` container rather than the legacy `openclaw`
      // container name. Falls back to OpenClaw if the CR can't be
      // read (e.g. local-only flow or AKS not configured) — which is
      // the safe default since legacy CRs imply OpenClaw.
      let runtimeKind: RuntimeKind = "OpenClaw";
      if (aksExists) {
        try {
          const { stdout: crJson } = await execa("kubectl", [
            "get", "clawsandbox", name, "-n", "azureclaw-system", "-o", "json",
          ], { stdio: "pipe" });
          runtimeKind = runtimeKindFromCr(JSON.parse(crJson));
        } catch { /* fall back to OpenClaw default */ }
      }
      const podContainer = agentContainerName(runtimeKind);

      // Ambiguity: both exist, no explicit flag
      if (localExists && aksExists && !options.local && !options.cloud) {
        console.log(chalk.yellow(`\n  ⚠️  '${name}' exists in both Docker and AKS:`));
        console.log(chalk.dim(`     Docker: ${localRunning ? "running" : "dormant (stopped)"}`));
        console.log(chalk.dim(`     AKS:    running`));
        console.log();
        console.log(`  ${chalk.cyan(`azureclaw connect ${name} --local`)}   → Docker`);
        console.log(`  ${chalk.cyan(`azureclaw connect ${name} --cloud`)}   → AKS`);
        console.log();
        // Auto-resolve: prefer cloud if local is dormant (handoff scenario)
        if (!localRunning) {
          console.log(chalk.dim(`  Auto-connecting to cloud (local is dormant)...\n`));
          options.cloud = true;
        } else {
          console.log(chalk.dim(`  Auto-connecting to local (running)...\n`));
          options.local = true;
        }
      }

      // Neither exists
      if (!localExists && !aksExists) {
        console.log(chalk.red(`\n  Sandbox '${name}' not found.`));
        console.log(chalk.dim(`  Run: azureclaw dev --name ${name}  (local) or  azureclaw up --name ${name}  (AKS)\n`));
        return;
      }

      // ── Local Docker mode ──
      const useLocal = options.local || (localExists && !aksExists);
      if (useLocal && localExists) {
        if (!localRunning) {
          console.log(chalk.yellow(`\n  Container '${name}' is stopped (dormant).`));
          console.log(chalk.dim(`  Start it with: docker start ${containerName}\n`));
          return;
        }
        if (!options.web) {
          console.log(chalk.hex("#0078D4")(`\n  Connected to ${chalk.bold(name)} (local). Agent is ready.\n`));
          console.log(chalk.dim(`  Chat:    openclaw tui`));
          console.log(chalk.dim(`  Message: openclaw agent --agent main --local -m "hello" --session-id test`));
          console.log(chalk.dim(`  Exit:    type "exit"\n`));
          await execa("docker", [
            "exec", "-it", containerName, "/bin/bash", "--login",
          ], { stdio: "inherit" });
          return;
        }
      }

      // ── AKS mode ──
      if (!aksExists) {
        console.log(chalk.red(`\n  Sandbox '${name}' not found on AKS.`));
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

        // Extract gateway token from the K8s Secret created by the controller.
        // The Secret is the source of truth — the openclaw container reads it
        // via env var OPENCLAW_GATEWAY_TOKEN. Reading the Secret here (instead
        // of `kubectl exec cat /sandbox/.bashrc`) is required by the
        // sandbox-exec-ban VAP and is also strictly better security: token
        // access is gated by namespaced RBAC on the Secret, no code-execution
        // path through the operator's cluster role.
        let gatewayToken = "";
        try {
          const { stdout: tokenB64 } = await execa("kubectl", [
            "get", "secret", "-n", namespace, "gateway-token",
            "-o", "jsonpath={.data.token}",
          ], { stdio: "pipe" });
          if (tokenB64.trim()) {
            gatewayToken = Buffer.from(tokenB64.trim(), "base64").toString("utf-8").trim();
          }
        } catch {
          console.log(chalk.yellow("  Could not read gateway-token Secret."));
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
            `deploy/${name}`, "-c", podContainer,
            "--", "/bin/bash", "--login",
          ], { stdio: "inherit" });
          return;
        }

        // Detect an already-running port-forward (the most common cause
        // of EADDRINUSE here is the user re-running `azureclaw connect`
        // while a previous invocation is still alive in another terminal,
        // or after `azureclaw dev` already opened the WebUI). If port
        // 18789 is open AND speaks HTTP, just print the URL + open the
        // browser instead of erroring out with a Node stack trace.
        const isPortServingHttp = await (async (): Promise<boolean> => {
          const net = await import("node:net");
          const tcpOpen = await new Promise<boolean>((resolve) => {
            const sock = net.createConnection({ host: "127.0.0.1", port: Number(localPort) });
            const done = (v: boolean) => { sock.destroy(); resolve(v); };
            sock.once("connect", () => done(true));
            sock.once("error", () => done(false));
            setTimeout(() => done(false), 500);
          });
          if (!tcpOpen) return false;
          try {
            // Any HTTP response (even 401/404) confirms it's an HTTP
            // server. fetch() with a short AbortController timeout.
            const ctrl = new AbortController();
            const t = setTimeout(() => ctrl.abort(), 1500);
            const r = await fetch(`http://127.0.0.1:${localPort}/`, { signal: ctrl.signal });
            clearTimeout(t);
            return r.status > 0;
          } catch {
            return false;
          }
        })();

        if (isPortServingHttp) {
          const url = `http://localhost:${localPort}/#token=${gatewayToken}`;
          console.log();
          console.log(chalk.dim(`  Port ${localPort} is already serving HTTP — reusing existing port-forward.`));
          console.log();
          console.log(`  ${chalk.green("→")} ${chalk.cyan.underline(url)}`);
          console.log();
          console.log(chalk.dim(`  Gateway token: ${gatewayToken}`));
          console.log();
          // Best-effort browser open; don't fail if it errors.
          try {
            const opener = process.platform === "darwin" ? "open"
              : process.platform === "win32" ? "start"
              : "xdg-open";
            await execa(opener, [url], { stdio: "ignore", detached: true });
          } catch { /* user can click the link */ }
          return;
        }

        // Start port-forward — pipe stderr so we can surface kubectl errors
        // when the connection drops. Without this, all the user sees is
        // "Disconnected." with no diagnostic.
        console.log(chalk.dim(`  Starting port-forward on localhost:${localPort}...`));
        const pf = execa("kubectl", [
          "port-forward", "-n", namespace,
          `deploy/${name}`, `${localPort}:18789`,
        ], { stdio: ["ignore", "pipe", "pipe"] });

        let pfStderr = "";
        pf.stderr?.on("data", (chunk: Buffer) => {
          const line = chunk.toString();
          pfStderr += line;
          // Surface kubectl errors live so the operator can see e.g. auth
          // failures, deploy-not-found, or LB resets immediately.
          if (/error|denied|unable|forbidden|refused|reset|EOF|lost connection/i.test(line)) {
            process.stderr.write(chalk.dim(`    [kubectl] ${line.trim()}\n`));
          }
        });

        // Wait for port-forward to be ready
        await new Promise(r => setTimeout(r, 2000));

        const url = `http://localhost:${localPort}/#token=${gatewayToken}`;
        console.log();
        console.log(`  ${chalk.green("→")} ${chalk.cyan.underline(url)}`);
        console.log();
        console.log(chalk.dim(`  Port-forward active. Press Ctrl+C to disconnect.\n`));

        // Keep alive until Ctrl+C
        const cleanup = () => {
          pf.kill("SIGTERM");
          console.log(chalk.dim("\n  Disconnected.\n"));
          process.exit(0);
        };
        process.on("SIGINT", cleanup);
        process.on("SIGTERM", cleanup);
        try {
          await pf;
        } catch {
          // port-forward exited
          console.log(chalk.dim("\n  Disconnected."));
          if (pfStderr.trim()) {
            console.log(chalk.dim(`  kubectl said:\n${pfStderr.split("\n").map(l => "    " + l).join("\n")}`));
          }
          console.log();
        } finally {
          process.removeListener("SIGINT", cleanup);
          process.removeListener("SIGTERM", cleanup);
        }
      } else {
        // Shell mode
        console.log(chalk.hex("#0078D4")(`\n  Connected to ${chalk.bold(name)}. Agent is ready.\n`));
        console.log(chalk.dim(`  Chat:    openclaw tui`));
        console.log(chalk.dim(`  Exit:    type "exit"\n`));
        await execa("kubectl", [
          "exec", "-it", "-n", namespace,
          `deploy/${name}`, "-c", podContainer,
          "--", "/bin/bash", "--login",
        ], { stdio: "inherit" });
      }
    });

  return cmd;
}
