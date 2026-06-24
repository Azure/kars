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
    .option("--reset", "Restart the openclaw deployment to clear gateway brute-force lockout (token is preserved)", false)
    .option("--context <name>", "kubectl context to use (auto-discovered if omitted)")
    .action(async (name: string, options: { shell: boolean; web: boolean; local: boolean; cloud: boolean; port: string; reset: boolean; context?: string }) => {
      const { execa } = await import("execa");
      const containerName = `kars-${name}`;
      const namespace = `kars-${name}`;
      const localPort = options.port;

      // Resolve the kubectl context to use. `kars list` works without
      // a current context because it explicitly probes every context;
      // `kars connect` did not, so users with no active context saw a
      // misleading "Sandbox not found" instead of the real cause. If
      // --context was passed, honor it; otherwise probe each context
      // until we find one where deploy/<name> exists in kars-<name>.
      const resolveContext = async (): Promise<string | undefined> => {
        if (options.context) return options.context;
        let contexts: string[] = [];
        try {
          const { stdout } = await execa("kubectl", ["config", "get-contexts", "-o", "name"], { stdio: "pipe" });
          contexts = stdout.trim().split("\n").filter(Boolean);
        } catch { return undefined; }
        // Try current context first (cheaper, common case).
        try {
          const { stdout: cur } = await execa("kubectl", ["config", "current-context"], { stdio: "pipe" });
          const c = cur.trim();
          if (c) contexts = [c, ...contexts.filter(x => x !== c)];
        } catch { /* no current context — fall through to probing all */ }
        for (const ctx of contexts) {
          try {
            await execa("kubectl", [
              "--context", ctx, "get", "deploy", name, "-n", namespace,
              "--request-timeout=3s", "--no-headers",
            ], { stdio: "pipe", timeout: 5000 });
            return ctx;
          } catch { /* not in this context, try next */ }
        }
        return undefined;
      };
      const kctx = options.cloud || !options.local ? await resolveContext() : undefined;
      const ctxArgs = kctx ? ["--context", kctx] : [];

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
            ...ctxArgs, "get", "deploy", name, "-n", namespace, "--no-headers",
          ], { stdio: "pipe" });
          aksExists = true;
        } catch { /* no AKS deployment */ }
      }

      // S10.A5: resolve container name from the live KarsSandbox CR
      // so non-OpenClaw runtimes (OpenAIAgents, MAF, BYO) hit the
      // generic `agent` container rather than the legacy `openclaw`
      // container name. Falls back to OpenClaw if the CR can't be
      // read (e.g. local-only flow or AKS not configured) — which is
      // the safe default since legacy CRs imply OpenClaw.
      let runtimeKind: RuntimeKind = "OpenClaw";
      if (aksExists) {
        try {
          const { stdout: crJson } = await execa("kubectl", [
            ...ctxArgs, "get", "karssandbox", name, "-n", "kars-system", "-o", "json",
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
        console.log(`  ${chalk.cyan(`kars connect ${name} --local`)}   → Docker`);
        console.log(`  ${chalk.cyan(`kars connect ${name} --cloud`)}   → AKS`);
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
        console.log(chalk.dim(`  Run: kars dev --name ${name}  (local) or  kars up --name ${name}  (AKS)\n`));
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
        console.log(chalk.dim(`  Run: kars up --name ${name}\n`));
        return;
      }

      // Runtime-specific connect path. OpenClaw exposes an HTTP WebUI on
      // port 18789 (gateway daemon) + a `gateway-token` Secret for bearer
      // auth. Hermes does not — its `hermes gateway run` daemon is for
      // channel dispatch (Telegram/Slack/etc), not a browser UI. For
      // Hermes (and future runtimes that don't ship a webui) we drop the
      // operator into an interactive `hermes` REPL via `kubectl exec -it`.
      // Same UX shape as `openclaw tui` for local Docker.
      //
      // The exec-ban VAP only targets the literal container name
      // `openclaw`; Hermes' container is `agent` so this is admission-
      // compliant (see deploy/helm/kars/templates/admission-pod-exec-ban.yaml
      // matchConditions).
      if (runtimeKind === "Hermes") {
        if (options.reset) {
          console.log(chalk.yellow("  Resetting Hermes pod (--reset)..."));
          try {
            await execa("kubectl", [
              ...ctxArgs, "rollout", "restart", "-n", namespace, `deploy/${name}`,
            ], { stdio: "pipe" });
            await execa("kubectl", [
              ...ctxArgs, "rollout", "status", "-n", namespace, `deploy/${name}`, "--timeout=120s",
            ], { stdio: "inherit" });
            console.log(chalk.green("  Pod restarted.\n"));
          } catch (e) {
            console.log(chalk.red(`  Reset failed: ${(e as Error).message}`));
            return;
          }
        }
        if (options.web) {
          console.log(chalk.yellow(`\n  Note: Hermes does not ship a browser WebUI (only OpenClaw does).`));
          console.log(chalk.dim(`  Falling back to interactive shell on the agent container...\n`));
        } else {
          console.log(chalk.hex("#0078D4")(`\n  Connecting to ${chalk.bold(name)} (Hermes). Interactive agent shell:\n`));
        }
        console.log(chalk.dim(`  Chat:    type your prompt + Enter`));
        console.log(chalk.dim(`  Exit:    Ctrl-D or 'exit'`));
        console.log(chalk.dim(`  Tools:   /tools, /help (hermes built-ins)\n`));
        // Set HOME + HERMES_HOME explicitly — kubectl exec does NOT
        // inherit container ENV, and the rootfs is read-only so
        // hermes' default ensure_hermes_home() fallback to /.hermes
        // would ENOENT. /sandbox is the writable emptyDir.
        //
        // `hermes chat` is the interactive REPL subcommand (`hermes`
        // alone prints usage). --accept-hooks lets the AGT
        // pre_tool_call hook run without per-tool confirmation
        // prompts — operator already approved by running `kars connect`.
        try {
          await execa("kubectl", [
            ...ctxArgs, "exec", "-it", "-n", namespace,
            `deploy/${name}`, "-c", podContainer,
            "--",
            "env", "HOME=/sandbox", "HERMES_HOME=/sandbox/.hermes",
            "hermes", "chat", "--accept-hooks",
          ], { stdio: "inherit" });
        } catch (e) {
          // exit-code 130 (Ctrl-D / Ctrl-C) is normal for an
          // interactive session — don't print a stack trace for it.
          const code = (e as { exitCode?: number }).exitCode;
          if (code !== 130 && code !== 0) {
            console.log(chalk.dim(`\n  Disconnected (exit code ${code ?? "?"}).`));
          } else {
            console.log(chalk.dim("\n  Disconnected.\n"));
          }
        }
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
            ...ctxArgs, "get", "pod", "-n", namespace, "-l", `kars.azure.com/sandbox=${name}`,
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
            ...ctxArgs, "get", "secret", "-n", namespace, "gateway-token",
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

        // --reset: rolling-restart the openclaw deployment to clear the
        // gateway's in-process brute-force lockout. The gateway token
        // Secret is reused across restarts (controller is idempotent),
        // so the URL/token printed below will still be valid after the
        // pod comes back. Useful when stale browser tabs from prior
        // `dev`/`up` runs have spammed the gateway with old tokens
        // and triggered "too many failed authentication attempts".
        if (options.reset) {
          console.log(chalk.yellow("  Resetting gateway lockout: restarting openclaw pod (token preserved)…"));
          try {
            await execa("kubectl", [
              ...ctxArgs, "rollout", "restart", "-n", namespace, `deploy/${name}`,
            ], { stdio: "pipe" });
            await execa("kubectl", [
              ...ctxArgs, "rollout", "status", "-n", namespace, `deploy/${name}`, "--timeout=120s",
            ], { stdio: "inherit" });
            console.log(chalk.green("  Gateway lockout cleared."));
            console.log(chalk.dim("  Tip: close any open browser tabs pointing at localhost:" + localPort + " before reopening — they may auto-reconnect with a stale token and re-trigger the lockout."));
            console.log();
          } catch (e) {
            console.log(chalk.red(`  Reset failed: ${(e as Error).message}`));
            return;
          }
        }

        if (isKata) {
          // Kata VMs don't support kubectl port-forward — use shell mode instead
          console.log(chalk.yellow("  Note: Kata VM pods don't support port-forward (known limitation)."));
          console.log(chalk.yellow("  The WebUI is accessible from inside the cluster only.\n"));
          console.log(chalk.dim(`  Gateway token: ${gatewayToken}`));
          console.log(chalk.dim(`  To access the WebUI, use an enhanced (non-Kata) sandbox:\n`));
          console.log(`  ${chalk.cyan(`kars up --skip-infra --isolation enhanced --name ${name}-web`)}`);
          console.log();
          console.log(chalk.dim("  Falling back to shell mode...\n"));
          await execa("kubectl", [
            ...ctxArgs, "exec", "-it", "-n", namespace,
            `deploy/${name}`, "-c", podContainer,
            "--", "/bin/bash", "--login",
          ], { stdio: "inherit" });
          return;
        }

        // Detect an already-running port-forward (the most common cause
        // of EADDRINUSE here is the user re-running `kars connect`
        // while a previous invocation is still alive in another terminal,
        // or after `kars dev` already opened the WebUI). If port
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
          console.log(chalk.dim(`  If WebUI says "too many failed authentication attempts": run 'kars connect ${name} --reset' to clear the gateway lockout (token is preserved).`));
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

        // Wait for the pod to actually be Running with the agent container
        // Ready before forwarding. `kubectl port-forward` fails hard with
        // "unable to forward port because pod is not running. Current
        // status=Pending" if the pod is still scheduling/pulling — which is
        // common right after `kars up` (the deploy finishes before the image
        // is pulled on the node). Poll until ready, fail fast on image-pull
        // errors, and print a clean message instead of a raw Node stack trace.
        const podReady = await (async (): Promise<{ ok: boolean; detail: string }> => {
          const deadlineMs = Date.now() + 180_000; // 3 min — first image pull on a fresh node can be slow
          const sleep = (ms: number) => new Promise(r => setTimeout(r, ms));
          let lastPhase = "Unknown";
          let printedWait = false;
          while (Date.now() < deadlineMs) {
            let pod: {
              status?: {
                phase?: string;
                containerStatuses?: { name: string; ready?: boolean; state?: { waiting?: { reason?: string; message?: string } } }[];
                initContainerStatuses?: { name: string; state?: { waiting?: { reason?: string; message?: string } } }[];
              };
            } | undefined;
            try {
              const { stdout } = await execa("kubectl", [
                ...ctxArgs, "get", "pod", "-n", namespace,
                "-l", `kars.azure.com/sandbox=${name}`,
                "--sort-by=.metadata.creationTimestamp", "-o", "json",
              ], { stdio: "pipe" });
              const items = JSON.parse(stdout).items as typeof pod[];
              pod = items[items.length - 1]; // newest pod (handles rollouts)
            } catch {
              await sleep(2500);
              continue;
            }
            if (!pod) { await sleep(2500); continue; }
            lastPhase = pod.status?.phase ?? "Unknown";
            const cs = (pod.status?.containerStatuses ?? []).find(c => c.name === podContainer);
            if (lastPhase === "Running" && cs?.ready === true) {
              if (printedWait) process.stdout.write("\n");
              return { ok: true, detail: "" };
            }
            // Fail fast on unrecoverable image / container errors (the agent
            // container OR the egress-guard init / inference-router sidecar).
            const waiters = [
              ...(pod.status?.initContainerStatuses ?? []),
              ...(pod.status?.containerStatuses ?? []),
            ];
            const fatal = waiters
              .map(c => ({ name: c.name, w: c.state?.waiting }))
              .find(x => x.w && /ImagePullBackOff|ErrImagePull|InvalidImageName|CreateContainerConfigError|CrashLoopBackOff/.test(x.w.reason ?? ""));
            if (fatal) {
              if (printedWait) process.stdout.write("\n");
              return { ok: false, detail: `${fatal.name}: ${fatal.w?.reason}${fatal.w?.message ? ` — ${fatal.w.message}` : ""}` };
            }
            const secsLeft = Math.max(0, Math.round((deadlineMs - Date.now()) / 1000));
            process.stdout.write(`\r  ${chalk.dim(`Waiting for ${name} to be ready (phase=${lastPhase}, ${secsLeft}s left)…   `)}`);
            printedWait = true;
            await sleep(3000);
          }
          if (printedWait) process.stdout.write("\n");
          return { ok: false, detail: `timed out after 3m (last phase=${lastPhase})` };
        })();

        if (!podReady.ok) {
          console.log();
          console.log(chalk.red(`  Sandbox '${name}' pod is not ready — ${podReady.detail}`));
          console.log(chalk.dim(`  Inspect it with:  kubectl describe pod -n ${namespace} -l kars.azure.com/sandbox=${name}`));
          console.log(chalk.dim(`  Then retry:       kars connect ${name}\n`));
          return;
        }

        // Pick a free local port. If the requested port is taken by something
        // that ISN'T a reusable HTTP forward (handled above), bump to the next
        // free port instead of crashing with EADDRINUSE — the original
        // "address already in use" footgun when 18789 is held by a stale
        // process or an unrelated listener.
        const effectivePort = await (async (): Promise<number> => {
          const net = await import("node:net");
          const canBind = (p: number) => new Promise<boolean>((resolve) => {
            const srv = net.createServer();
            srv.once("error", () => resolve(false));
            srv.once("listening", () => srv.close(() => resolve(true)));
            srv.listen(p, "127.0.0.1");
          });
          const start = Number(localPort);
          for (let p = start; p < start + 20; p++) {
            if (await canBind(p)) return p;
          }
          return start; // give up — let kubectl surface the bind error
        })();
        if (effectivePort !== Number(localPort)) {
          console.log(chalk.dim(`  Port ${localPort} is in use — using ${effectivePort} instead.`));
        }

        console.log(chalk.dim(`  Starting port-forward on localhost:${effectivePort}...`));
        const pf = execa("kubectl", [
          ...ctxArgs, "port-forward", "-n", namespace,
          `deploy/${name}`, `${effectivePort}:18789`,
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

        const url = `http://localhost:${effectivePort}/#token=${gatewayToken}`;
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
