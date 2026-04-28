import { Command } from "commander";
import chalk from "chalk";
import { Stepper, banner, section, kvLine, checkLine } from "../stepper.js";
import type { ChildProcess } from "node:child_process";

/**
 * azureclaw handoff — live agent migration between local and cloud.
 *
 * OPERATOR-MODE ORCHESTRATION (CLI-driven)
 * This command is for direct operator use from the terminal. It calls router
 * endpoints directly (POST /init, /snapshot, /drain, etc.) without the
 * two-stage confirmation gate used by the LLM-driven path.
 *
 * The LLM-driven path lives in plugin.ts (azureclaw_handoff_request →
 * azureclaw_handoff_confirm → _runHandoffOrchestration). That path uses
 * the POST /pending + /confirm two-stage gate, transfers state via E2E mesh
 * (Signal Protocol), and reports progress via azureclaw_handoff_status.
 *
 * Both paths are intentional — CLI for operators, plugin for interactive
 * webchat. See docs/architecture-diagrams.md §11.5 for the comparison.
 *
 * Forward:  azureclaw handoff <name> --to cloud
 * Reverse:  azureclaw handoff <name> --to local
 * Status:   azureclaw handoff <name> --status
 * Abort:    azureclaw handoff <name> --abort
 */
export function handoffCommand(): Command {
  const cmd = new Command("handoff");

  cmd
    .description("Live-migrate an agent between local Docker and AKS (handoff)")
    .argument("<name>", "Sandbox name")
    .option("--to <target>", "Handoff target: cloud or local")
    .option("--status", "Show current handoff status", false)
    .option("--abort", "Abort an in-progress handoff", false)
    .action(async (name: string, options: { to?: string; status: boolean; abort: boolean }) => {
      const { execa } = await import("execa");
      const containerName = `azureclaw-${name}`;

      // Shared tar command for workspace + config collection.
      // Captures: workspace, gateway config, cron jobs, governance policies, agent state.
      // Excludes: compiled extensions (regenerable), node_modules, git, python cache.
      const WORKSPACE_TAR_CMD =
        "tar czf - -C /sandbox " +
        "--exclude='.openclaw/extensions/*/dist' --exclude='.openclaw/extensions/*/node_modules' " +
        "--exclude='node_modules' --exclude='.git' " +
        "--exclude='*.pyc' --exclude='__pycache__' " +
        ".openclaw/workspace .openclaw/openclaw.json .openclaw/cron " +
        ".openclaw/policies .openclaw/agents 2>/dev/null | base64 -w0";

      // ── Helper: call the router inside the container ────────────
      async function routerExec(
        method: string,
        path: string,
        body?: unknown,
        extraHeaders?: Record<string, string>,
      ): Promise<{ status: number; body: any }> {
        const curlArgs = [
          "exec", containerName,
          "curl", "-sf", "--max-time", "30",
          "-X", method,
          "-H", "Content-Type: application/json",
        ];
        if (extraHeaders) {
          for (const [k, v] of Object.entries(extraHeaders)) {
            curlArgs.push("-H", `${k}: ${v}`);
          }
        }
        if (body) {
          curlArgs.push("-d", JSON.stringify(body));
        }
        curlArgs.push("-w", "\n%{http_code}");
        curlArgs.push(`http://127.0.0.1:8443${path}`);

        const { stdout } = await execa("docker", curlArgs, { stdio: "pipe" });
        const lines = stdout.trimEnd().split("\n");
        const statusCode = parseInt(lines[lines.length - 1], 10);
        const responseBody = lines.slice(0, -1).join("\n");
        try {
          return { status: statusCode, body: JSON.parse(responseBody) };
        } catch {
          return { status: statusCode, body: { raw: responseBody } };
        }
      }

      // Read admin token from the container env
      async function getAdminToken(): Promise<string | undefined> {
        try {
          const { stdout } = await execa("docker", [
            "exec", containerName,
            "printenv", "ADMIN_TOKEN",
          ], { stdio: "pipe" });
          return stdout.trim() || undefined;
        } catch {
          // Try reading from the secrets file
          try {
            const { stdout } = await execa("docker", [
              "exec", containerName,
              "cat", "/run/secrets/admin-token",
            ], { stdio: "pipe" });
            return stdout.trim() || undefined;
          } catch {
            // Fallback: entrypoint saves the token to /tmp/.agt-admin-token
            try {
              const { stdout } = await execa("docker", [
                "exec", containerName,
                "cat", "/tmp/.agt-admin-token",
              ], { stdio: "pipe" });
              return stdout.trim() || undefined;
            } catch {
              return undefined;
            }
          }
        }
      }

      // ── Helper: port-forward to AKS pod and call its router ────
      const targetNs = `azureclaw-${name}`;
      let aksPfProc: ChildProcess | undefined;
      let aksPfPort = 18445; // temp local port for source AKS pod

      async function aksPortForwardStart(): Promise<void> {
        if (aksPfProc) return; // already running
        const { execa: ex } = await import("execa");
        aksPfProc = ex("kubectl", [
          "port-forward", "-n", targetNs,
          `svc/${name}`, `${aksPfPort}:8443`,
        ], { stdio: "pipe", reject: false }) as any;
        // Wait for port-forward to be ready
        const http = await import("node:http");
        for (let i = 0; i < 15; i++) {
          await new Promise(r => setTimeout(r, 1000));
          try {
            const ok: boolean = await new Promise((resolve) => {
              const req = http.get(`http://127.0.0.1:${aksPfPort}/readyz`, { timeout: 2000 }, (res) => {
                let data = "";
                res.on("data", (c: Buffer) => { data += c.toString(); });
                res.on("end", () => resolve(res.statusCode === 200));
              });
              req.on("error", () => resolve(false));
              req.on("timeout", () => { req.destroy(); resolve(false); });
            });
            if (ok) return;
          } catch { /* retry */ }
        }
        throw new Error("AKS port-forward not ready after 15s");
      }

      function aksPortForwardStop(): void {
        if (aksPfProc) {
          try { (aksPfProc as any).kill(); } catch { /* ignore */ }
          aksPfProc = undefined;
        }
      }

      async function aksRouterExec(
        method: string,
        path: string,
        body?: unknown,
        extraHeaders?: Record<string, string>,
      ): Promise<{ status: number; body: any }> {
        const http = await import("node:http");
        const payload = body ? JSON.stringify(body) : undefined;
        return new Promise((resolve, reject) => {
          const opts: any = {
            hostname: "127.0.0.1",
            port: aksPfPort,
            path,
            method,
            headers: {
              "Content-Type": "application/json",
              ...(extraHeaders || {}),
              ...(payload ? { "Content-Length": Buffer.byteLength(payload) } : {}),
            },
            timeout: 30000,
          };
          const req = http.request(opts, (res: any) => {
            let data = "";
            res.on("data", (c: Buffer) => { data += c.toString(); });
            res.on("end", () => {
              try { resolve({ status: res.statusCode, body: JSON.parse(data) }); }
              catch { resolve({ status: res.statusCode, body: { raw: data } }); }
            });
          });
          req.on("error", reject);
          req.on("timeout", () => { req.destroy(); reject(new Error("timeout")); });
          if (payload) req.write(payload);
          req.end();
        });
      }

      // ── Helper: get admin token from AKS pod ───────────────────
      async function getAksAdminToken(): Promise<string | undefined> {
        const { execa: ex } = await import("execa");
        // On AKS, the controller mounts the admin token at /etc/azureclaw/secrets/admin-token
        try {
          const { stdout } = await ex("kubectl", [
            "exec", "-n", targetNs,
            `deploy/${name}`, "-c", "inference-router", "--",
            "cat", "/etc/azureclaw/secrets/admin-token",
          ], { stdio: "pipe", reject: false });
          if (stdout.trim()) return stdout.trim();
        } catch { /* fallback */ }
        // Fallback: try the openclaw container (same volume mount)
        try {
          const { stdout } = await ex("kubectl", [
            "exec", "-n", targetNs,
            `deploy/${name}`, "-c", "openclaw", "--",
            "cat", "/etc/azureclaw/secrets/admin-token",
          ], { stdio: "pipe", reject: false });
          if (stdout.trim()) return stdout.trim();
        } catch { /* fallback */ }
        // Fallback: read from K8s secret directly
        try {
          const { stdout } = await ex("kubectl", [
            "get", "secret", "router-admin-token", "-n", targetNs,
            "-o", "jsonpath={.data.token}",
          ], { stdio: "pipe", reject: false });
          if (stdout.trim()) {
            return Buffer.from(stdout.trim(), "base64").toString("utf8").trim();
          }
        } catch { /* no secret */ }
        return undefined;
      }

      // ── Helper: wake dormant local Docker container ─────────────
      async function wakeDormantDocker(): Promise<{ ready: boolean; error?: string }> {
        const { execa: ex } = await import("execa");
        // Check if container exists
        let containerState: string | undefined;
        try {
          const { stdout } = await ex("docker", [
            "inspect", "-f", "{{.State.Status}}", containerName,
          ], { stdio: "pipe" });
          containerState = stdout.trim();
        } catch {
          return { ready: false, error: `Container '${containerName}' not found. Run 'azureclaw dev --name ${name}' first.` };
        }

        // Start if stopped/exited
        if (containerState === "exited" || containerState === "created") {
          try {
            await ex("docker", ["start", containerName], { stdio: "pipe" });
          } catch (e: any) {
            return { ready: false, error: `Failed to start container: ${e.message}` };
          }
        } else if (containerState !== "running") {
          return { ready: false, error: `Container in unexpected state: ${containerState}` };
        }

        // Wait for router to be healthy (up to 30s)
        for (let i = 0; i < 30; i++) {
          try {
            await ex("docker", [
              "exec", containerName, "sh", "-c",
              "wget -qO- --timeout=2 http://127.0.0.1:8443/readyz 2>/dev/null || curl -sf --max-time 2 http://127.0.0.1:8443/readyz 2>/dev/null",
            ], { stdio: "pipe" });
            return { ready: true };
          } catch { /* not ready yet */ }
          await new Promise(r => setTimeout(r, 1000));
        }
        return { ready: false, error: "Local router not healthy after 30s" };
      }

      // ── Helper: read AKS CRD spec to inherit settings ──────────
      async function readAksCrdSpec(): Promise<{
        model: string; learnEgress: boolean; isolation: string; trustThreshold: number;
      }> {
        const defaults = { model: "gpt-5.4", learnEgress: true, isolation: "enhanced", trustThreshold: 500 };
        try {
          const { execa: ex } = await import("execa");
          const { stdout } = await ex("kubectl", [
            "get", "clawsandbox", name, "-n", "azureclaw-system",
            "-o", "jsonpath={.spec}",
          ], { stdio: "pipe" });
          const spec = JSON.parse(stdout);
          return {
            model: spec.inference?.model || spec.runtime?.openclaw?.config?.agent?.model?.replace("azure/", "") || defaults.model,
            learnEgress: spec.networkPolicy?.learnEgress ?? defaults.learnEgress,
            isolation: spec.sandbox?.isolation || defaults.isolation,
            trustThreshold: spec.governance?.trustThreshold || defaults.trustThreshold,
          };
        } catch {
          return defaults;
        }
      }

      // ── Helper: re-hydrate credentials from K8s secret to Docker ─
      async function rehydrateCredentials(): Promise<string[]> {
        const injected: string[] = [];
        try {
          const { execa: ex } = await import("execa");
          const { stdout } = await ex("kubectl", [
            "get", "secret", `${name}-credentials`, "-n", targetNs,
            "-o", "json",
          ], { stdio: "pipe" });
          const secret = JSON.parse(stdout);
          const data = secret.data || {};
          for (const [key, b64] of Object.entries(data)) {
            const val = Buffer.from(b64 as string, "base64").toString("utf8");
            if (val) {
              // Inject into running Docker container via a temp env file
              // docker exec doesn't support -e, so write to a known location
              // and have the entrypoint source it. Alternatively, we set it
              // via the router's admin API if available.
              // Simplest: use docker exec to export into the container's env
              // by writing to /tmp/.handoff-credentials
              injected.push(key);
            }
          }
          if (injected.length > 0) {
            // Write all credentials as env exports into a file the entrypoint sources
            const envLines = Object.entries(data)
              .map(([k, b64]) => `${k}=${Buffer.from(b64 as string, "base64").toString("utf8")}`)
              .join("\n");
            await ex("docker", [
              "exec", containerName, "sh", "-c",
              `cat > /tmp/.handoff-credentials << 'CRED_EOF'\n${envLines}\nCRED_EOF`,
            ], { stdio: "pipe" });
          }
        } catch {
          // K8s secret not available or no kubectl — fall back silently
          // Local secrets.json will be used by the entrypoint
        }
        return injected;
      }

      // ── STATUS ──────────────────────────────────────────────────
      if (options.status) {
        try {
          const adminToken = await getAdminToken();
          const headers: Record<string, string> = {};
          if (adminToken) headers["Authorization"] = `Bearer ${adminToken}`;

          const resp = await routerExec("GET", "/agt/handoff/status", undefined, headers);
          const s = resp.body;

          banner("AzureClaw · Handoff Status", name);

          kvLine("Phase", s.phase || "idle");
          kvLine("Direction", s.direction || "—");
          kvLine("Registry mode", s.registry_mode || "unknown");
          kvLine("Handoff available", s.handoff_available ? chalk.green("yes") : chalk.yellow("no (requires --global-registry)"));
          if (s.predecessor_amid) kvLine("Predecessor", s.predecessor_amid);
          if (s.successor_amid) kvLine("Successor", s.successor_amid);
          if (s.snapshot_size_bytes) kvLine("Snapshot size", `${(s.snapshot_size_bytes / 1024).toFixed(1)} KB`);
          if (s.draining) kvLine("Draining", `${s.drain_duration_secs || 0}s`);
          if (s.error) kvLine("Error", chalk.red(s.error));

          console.log();
        } catch (e: any) {
          console.log(chalk.red(`\n  Could not reach sandbox '${name}': ${e.message}\n`));
          process.exit(1);
        }
        return;
      }

      // ── ABORT ───────────────────────────────────────────────────
      if (options.abort) {
        try {
          const adminToken = await getAdminToken();
          if (!adminToken) {
            console.log(chalk.red("\n  Cannot abort: admin token not found.\n"));
            process.exit(1);
          }

          // Need both admin and handoff tokens
          const statusResp = await routerExec("GET", "/agt/handoff/status", undefined, {
            Authorization: `Bearer ${adminToken}`,
          });

          if (!statusResp.body.handoff_token_active) {
            console.log(chalk.yellow(`\n  No active handoff to abort (phase: ${statusResp.body.phase}).\n`));
            return;
          }

          console.log(chalk.yellow(`\n  Aborting handoff (current phase: ${statusResp.body.phase})...`));

          // The abort endpoint requires the handoff token, which only the
          // initiating CLI process has. If we don't have it, we can't abort
          // from a different terminal. Show guidance instead.
          console.log(chalk.dim("  Note: abort must be called from the terminal that initiated the handoff."));
          console.log(chalk.dim(`  The handoff token is held in that process's memory.\n`));
        } catch (e: any) {
          console.log(chalk.red(`\n  Abort failed: ${e.message}\n`));
          process.exit(1);
        }
        return;
      }

      // ── FORWARD / REVERSE HANDOFF ──────────────────────────────
      if (!options.to) {
        console.log(chalk.red("\n  Specify --to cloud or --to local (or --status / --abort).\n"));
        console.log(chalk.dim(`  Examples:`));
        console.log(chalk.dim(`    azureclaw handoff ${name} --to cloud    # migrate to AKS`));
        console.log(chalk.dim(`    azureclaw handoff ${name} --to local    # migrate back`));
        console.log(chalk.dim(`    azureclaw handoff ${name} --status      # check progress\n`));
        process.exit(1);
      }

      const direction = options.to === "local" ? "aks_to_local" : "local_to_aks";
      const directionLabel = direction === "local_to_aks" ? "Local → Cloud" : "Cloud → Local";

      banner("AzureClaw · Agent Handoff", directionLabel);

      const stepper = new Stepper({ totalSteps: direction === "aks_to_local" ? 13 : 7 });

      try {
        // Direction-aware source router: forward talks to Docker, reverse to AKS
        const isReverse = direction === "aks_to_local";

        // For reverse: establish port-forward to AKS before any source operations
        if (isReverse) {
          stepper.step("Connecting to cloud agent...");
          await aksPortForwardStart();
          stepper.done("Connected to AKS via port-forward");
        }

        // sourceExec talks to the SOURCE agent's router (Docker or AKS)
        const sourceExec = isReverse ? aksRouterExec : routerExec;

        // Step 1: Verify source agent is running
        stepper.step("Verifying source agent...");
        const adminToken = isReverse ? await getAksAdminToken() : await getAdminToken();
        if (!adminToken) {
          stepper.fail("Admin token not found — cannot initiate handoff");
          process.exit(1);
        }

        const authHeaders = { Authorization: `Bearer ${adminToken}` };

        // Check handoff status (also verifies connectivity + registry mode)
        const statusResp = await sourceExec("GET", "/agt/handoff/status", undefined, authHeaders);
        if (statusResp.status >= 400) {
          stepper.fail(`Router returned ${statusResp.status}`);
          process.exit(1);
        }

        const handoffAvailable = statusResp.body.handoff_available;
        const registryMode = statusResp.body.registry_mode;

        if (!handoffAvailable) {
          stepper.fail("Handoff requires global registry mode");
          console.log(chalk.yellow(`
  Current registry mode: ${chalk.bold(registryMode)}

  To enable handoff, restart with a global registry:
    ${chalk.cyan(`azureclaw dev --global-registry <registry-url> --name ${name}`)}

  The global registry must be accessible from both local and cloud environments.
`));
          process.exit(1);
        }

        stepper.done(`Source agent verified (registry: ${registryMode})`);

        // Step 2: Initialize handoff — get one-time token
        stepper.step("Initializing handoff...");
        const initResp = await sourceExec("POST", "/agt/handoff/init", {
          direction,
          ttl_seconds: 300,
        }, authHeaders);

        if (initResp.status >= 400) {
          const errMsg = initResp.body.error || `HTTP ${initResp.status}`;
          stepper.fail(`Init failed: ${errMsg}`);
          if (initResp.body.hint) console.log(chalk.dim(`\n  ${initResp.body.hint}\n`));
          process.exit(1);
        }

        const handoffToken = initResp.body.handoff_token;
        const tokenHash = initResp.body.token_hash;
        const handoffHeaders = {
          ...authHeaders,
          "X-Handoff-Token": handoffToken,
        };

        stepper.done(`Handoff initialized (token: ${tokenHash?.slice(0, 8)}...)`);

        // Step 3: Create encrypted snapshot
        stepper.step("Creating state snapshot...");

        // Build a shared secret for encryption. In production this would come
        // from a DH key exchange between source and target agents. For now,
        // we derive it from the admin token + handoff token (both are secrets
        // known only to the CLI process).
        const crypto = await import("node:crypto");
        const sharedSecret = crypto
          .createHash("sha256")
          .update(`${adminToken}:${handoffToken}`)
          .digest("base64");

        // Collect workspace, chat/memory, and credentials from the source agent
        // so the snapshot includes full state (not just trust/audit from the router).
        const snapshotPayload: Record<string, unknown> = { shared_secret: sharedSecret };
        // Collected in forward path, used later to create K8s secret on AKS target
        const credentialValues: Array<{ env_key: string; value: string }> = [];

        if (isReverse) {
          // Reverse: source is AKS — exec into openclaw container for workspace
          try {
            const { stdout: tarB64 } = await execa("kubectl", [
              "exec", "-n", targetNs, "-c", "openclaw",
              `deploy/${name}`, "--",
              "sh", "-c", WORKSPACE_TAR_CMD,
            ], { stdio: "pipe", timeout: 15000 });
            if (tarB64.length > 0 && tarB64.length < 50 * 1024 * 1024) {
              snapshotPayload.workspace_tar = tarB64;
            }
          } catch { /* workspace collection is best-effort */ }

          // Collect memory items from Foundry via the AKS router
          try {
            const store = `memory-${name}`;
            const apiVer = "api-version=2025-11-15-preview";
            const memResp = await sourceExec("POST",
              `/memory_stores/${store}:search_memories?${apiVer}`,
              { scope: name, options: { max_memories: 20 } },
              handoffHeaders);
            if (memResp.status === 200 && memResp.body?.memories?.length) {
              const chatContext = memResp.body.memories.map((m: any) => ({
                role: "assistant",
                content: m.memory_item?.content || m.content || m.text || JSON.stringify(m),
                timestamp: m.created_at || new Date().toISOString(),
              }));
              snapshotPayload.chat_snapshot = Buffer.from(JSON.stringify(chatContext)).toString("base64");
            }
          } catch { /* memory collection is best-effort */ }

          // Credential refs from AKS container environment
          try {
            const { stdout: envOut } = await execa("kubectl", [
              "exec", "-n", targetNs, "-c", "openclaw",
              `deploy/${name}`, "--",
              "sh", "-c", "env",
            ], { stdio: "pipe", timeout: 5000 });
            const credMap: Array<[string, string]> = [
              ["TELEGRAM_BOT_TOKEN", "telegram"], ["SLACK_BOT_TOKEN", "slack"],
              ["DISCORD_BOT_TOKEN", "discord"], ["BRAVE_API_KEY", "brave"],
              ["TAVILY_API_KEY", "tavily"],
            ];
            const credRefs: Array<{ name: string; env_key: string }> = [];
            for (const [envKey, label] of credMap) {
              if (envOut.includes(`${envKey}=`)) credRefs.push({ name: label, env_key: envKey });
            }
            if (credRefs.length > 0) snapshotPayload.credentials = credRefs;
          } catch { /* credential scan is best-effort */ }
        } else {
          // Forward: source is local Docker — exec into container for workspace
          try {
            const { stdout: tarB64 } = await execa("docker", [
              "exec", containerName, "sh", "-c", WORKSPACE_TAR_CMD,
            ], { stdio: "pipe", timeout: 15000 });
            if (tarB64.length > 0 && tarB64.length < 50 * 1024 * 1024) {
              snapshotPayload.workspace_tar = tarB64;
            }
          } catch { /* workspace collection is best-effort */ }

          // Collect memory items from Foundry via the local router
          try {
            const store = `memory-${name}`;
            const apiVer = "api-version=2025-11-15-preview";
            const memResp = await sourceExec("POST",
              `/memory_stores/${store}:search_memories?${apiVer}`,
              { scope: name, options: { max_memories: 20 } },
              handoffHeaders);
            if (memResp.status === 200 && memResp.body?.memories?.length) {
              const chatContext = memResp.body.memories.map((m: any) => ({
                role: "assistant",
                content: m.memory_item?.content || m.content || m.text || JSON.stringify(m),
                timestamp: m.created_at || new Date().toISOString(),
              }));
              snapshotPayload.chat_snapshot = Buffer.from(JSON.stringify(chatContext)).toString("base64");
            }
          } catch { /* memory collection is best-effort */ }

          // Credential refs + values from local Docker container
          try {
            const { stdout: envOut } = await execa("docker", [
              "exec", containerName, "sh", "-c", "env",
            ], { stdio: "pipe", timeout: 5000 });
            const credMap: Array<[string, string]> = [
              ["TELEGRAM_BOT_TOKEN", "telegram"], ["SLACK_BOT_TOKEN", "slack"],
              ["DISCORD_BOT_TOKEN", "discord"], ["BRAVE_API_KEY", "brave"],
              ["TAVILY_API_KEY", "tavily"],
            ];
            const credRefs: Array<{ name: string; env_key: string }> = [];
            for (const [envKey, label] of credMap) {
              const match = envOut.match(new RegExp(`^${envKey}=(.+)$`, "m"));
              if (match) {
                credRefs.push({ name: label, env_key: envKey });
                credentialValues.push({ env_key: envKey, value: match[1] });
              }
            }
            if (credRefs.length > 0) snapshotPayload.credentials = credRefs;
          } catch { /* credential scan is best-effort */ }
        }

        // Collect sub-agent snapshots (best-effort — works in both K8s and Docker)
        const subAgentNames: string[] = [];
        try {
          const subResp = await sourceExec("GET", "/agt/handoff/sub-agents", undefined, authHeaders);
          if (subResp.status === 200 && subResp.body?.count > 0) {
            const subSnaps = subResp.body.sub_agent_snapshots as Array<{
              name: string; workspace_tar: string; [k: string]: unknown;
            }>;
            subAgentNames.push(...subSnaps.map(s => s.name));

            // Phase 1: Signal all sub-agents to save in-progress work
            for (const snap of subSnaps) {
              try {
                const interruptCmd = `mkdir -p /sandbox/.openclaw/workspace && echo '{"reason":"parent_handoff","time":"${new Date().toISOString()}"}' > /sandbox/.openclaw/workspace/.handoff-interrupt`;
                if (isReverse) {
                  const subNs = `azureclaw-${snap.name}`;
                  await execa("kubectl", [
                    "exec", "-n", subNs, "-c", "openclaw",
                    `deploy/${snap.name}`, "--", "sh", "-c", interruptCmd,
                  ], { stdio: "pipe", timeout: 5000, reject: false });
                } else {
                  const containerName = `azureclaw-${snap.name}`;
                  await execa("docker", [
                    "exec", containerName, "sh", "-c", interruptCmd,
                  ], { stdio: "pipe", timeout: 5000, reject: false });
                }
              } catch { /* interrupt signal is best-effort */ }
            }
            // Brief pause for sub-agents to notice the interrupt file
            await new Promise(r => setTimeout(r, 3000));

            // Phase 2: Collect workspaces (sub-agents may have saved progress)
            for (const snap of subSnaps) {
              try {
                const subNs = `azureclaw-${snap.name}`;

                let tarB64 = "";
                if (isReverse) {
                  // Source is AKS — kubectl exec into sub-agent's openclaw container
                  const { stdout } = await execa("kubectl", [
                    "exec", "-n", subNs, "-c", "openclaw",
                    `deploy/${snap.name}`, "--", "sh", "-c", WORKSPACE_TAR_CMD,
                  ], { stdio: "pipe", timeout: 10000 });
                  tarB64 = stdout;
                } else {
                  // Source is local Docker — docker exec into sub-agent container
                  const containerName = `azureclaw-${snap.name}`;
                  const { stdout } = await execa("docker", [
                    "exec", containerName, "sh", "-c", WORKSPACE_TAR_CMD,
                  ], { stdio: "pipe", timeout: 10000 });
                  tarB64 = stdout;
                }

                if (tarB64.length > 0 && tarB64.length < 2 * 1024 * 1024) {
                  snap.workspace_tar = tarB64;
                }
              } catch { /* sub-agent workspace collection is best-effort */ }
            }

            snapshotPayload.sub_agent_snapshots = subSnaps;
          }
        } catch { /* sub-agent collection is best-effort */ }

        const snapshotResp = await sourceExec("POST", "/agt/handoff/snapshot",
          snapshotPayload, handoffHeaders);

        if (snapshotResp.status >= 400) {
          stepper.fail(`Snapshot failed: ${snapshotResp.body.error || `HTTP ${snapshotResp.status}`}`);
          // Try to abort
          await sourceExec("POST", "/agt/handoff/abort", {}, handoffHeaders).catch(() => {});
          process.exit(1);
        }

        const snapshotSize = snapshotResp.body.snapshot_size_bytes || 0;
        const snapshotItems = snapshotResp.body.items || {};
        stepper.done(`Snapshot created (${(snapshotSize / 1024).toFixed(1)} KB)`);

        // Step 4: Drain — stop accepting new work
        stepper.step("Draining active work...");
        const drainResp = await sourceExec("POST", "/agt/handoff/drain", {}, handoffHeaders);

        if (drainResp.status >= 400) {
          stepper.fail(`Drain failed: ${drainResp.body.error || `HTTP ${drainResp.status}`}`);
          await sourceExec("POST", "/agt/handoff/abort", {}, handoffHeaders).catch(() => {});
          process.exit(1);
        }

        stepper.done("Source agent drained (no new work accepted)");

        // Step 5: Transfer to target

        if (direction === "local_to_aks") {
          // ── H4: Provision target on AKS via ClawSandbox CRD ──────────────
          stepper.step("Transferring state to target...");
          // 1. Apply a ClawSandbox CRD for the target agent
          const targetName = name; // same name on AKS
          const targetNs = `azureclaw-${targetName}`;

          // Inherit the source agent's settings — cloud target should match parent
          let sourceIsolation = "enhanced";
          let sourceLearnEgress = true;
          let sourceTrustThreshold = 500;
          try {
            const { stdout: envOut } = await execa("docker", [
              "exec", containerName, "printenv",
            ], { stdio: "pipe", reject: false });
            for (const line of envOut.split("\n")) {
              if (line.startsWith("EGRESS_LEARN_MODE=")) {
                sourceLearnEgress = line.split("=")[1]?.trim().toLowerCase() === "true";
              } else if (line.startsWith("SANDBOX_ISOLATION=")) {
                sourceIsolation = line.split("=")[1]?.trim() || "enhanced";
              } else if (line.startsWith("AGT_TRUST_THRESHOLD=")) {
                const val = parseInt(line.split("=")[1]?.trim(), 10);
                if (!isNaN(val)) sourceTrustThreshold = val;
              }
            }
          } catch { /* use safe defaults */ }

          // Always apply the CRD (create or update) with inherited config.
          // Server-side apply is idempotent; the controller only restarts the
          // pod if the deployment spec actually changed.
          const crdManifest = JSON.stringify({
            apiVersion: "azureclaw.io/v1alpha1",
            kind: "ClawSandbox",
            metadata: { name: targetName, namespace: "azureclaw-system" },
            spec: {
              model: process.env.DEFAULT_MODEL || "gpt-5.4",
              handoff: { mode: "restore", predecessor: name },
              networkPolicy: {
                defaultDeny: true,
                approvalRequired: true,
                learnEgress: sourceLearnEgress,
              },
              sandbox: {
                isolation: sourceIsolation,
              },
              governance: {
                enabled: true,
                toolPolicy: "default",
                trustThreshold: sourceTrustThreshold,
              },
            },
          });
          try {
            await execa("kubectl", ["apply", "-f", "-"], {
              input: crdManifest,
              stdio: ["pipe", "pipe", "pipe"],
            });
          } catch (e: any) {
            stepper.fail(`Failed to create target sandbox CRD: ${e.message}`);
            await routerExec("POST", "/agt/handoff/abort", {}, handoffHeaders).catch(() => {});
            process.exit(1);
          }

          // Rehydrate credentials from Docker into K8s secret so the target
          // pod's envFrom can mount them. Must happen before pod starts.
          if (credentialValues.length > 0) {
            stepper.step(`Migrating ${credentialValues.length} credential(s) to AKS...`);
            try {
              const secretName = `${targetName}-credentials`;
              const secretArgs = [
                "create", "secret", "generic", secretName,
                "-n", targetNs, "--dry-run=client", "-o", "yaml",
              ];
              for (const cred of credentialValues) {
                secretArgs.push(`--from-literal=${cred.env_key}=${cred.value}`);
              }
              const { stdout: yaml } = await execa("kubectl", secretArgs, { stdio: "pipe" });
              await execa("kubectl", ["apply", "-f", "-"], { input: yaml, stdio: ["pipe", "pipe", "pipe"] });
              stepper.done(`Credentials migrated (${credentialValues.map(c => c.env_key).join(", ")})`);
            } catch (e: any) {
              stepper.warn(`Credential migration failed (non-fatal): ${e.message}`);
            }
          }

          // Check if the pod already existed (need to wait or it's already running)
          let targetExists = false;
          try {
            const { stdout } = await execa("kubectl", [
              "get", "pods", "-n", targetNs,
              "-l", `app.kubernetes.io/name=${targetName}`,
              "-o", "jsonpath={.items[0].status.conditions[?(@.type=='Ready')].status}",
            ], { stdio: "pipe", reject: false });
            targetExists = stdout.trim() === "True";
          } catch { /* no pod yet */ }

          // Wait for target pod to be ready (up to 120s)
          stepper.step("Waiting for target pod on AKS...");
          let targetReady = false;
          for (let i = 0; i < 60; i++) {
            try {
              const { stdout } = await execa("kubectl", [
                "get", "pods", "-n", targetNs,
                "-l", `app.kubernetes.io/name=${targetName}`,
                "-o", "jsonpath={.items[0].status.conditions[?(@.type=='Ready')].status}",
              ], { stdio: "pipe", reject: false });
              if (stdout.trim() === "True") {
                targetReady = true;
                break;
              }
            } catch { /* not ready yet */ }
            await new Promise(r => setTimeout(r, 2000));
          }

          if (!targetReady) {
            stepper.fail("Target pod not ready after 120s");
            await routerExec("POST", "/agt/handoff/abort", {}, handoffHeaders).catch(() => {});
            process.exit(1);
          }

          // Port-forward to the target's router to send the restore payload
          // The target agent's router listens on 8443 inside the pod
          const targetPort = 18444; // temp local port for target
          const pfProc = execa("kubectl", [
            "port-forward", "-n", targetNs,
            `svc/${targetName}`, `${targetPort}:8443`,
          ], { stdio: "pipe", reject: false });

          // Wait for port-forward to be ready
          await new Promise(r => setTimeout(r, 3000));

          try {
            // Get the encrypted snapshot blob from the source
            const blobResp = await routerExec("POST", "/agt/handoff/snapshot", {}, handoffHeaders);
            if (blobResp.status >= 400) {
              stepper.fail(`Failed to retrieve snapshot: ${blobResp.body.error || `HTTP ${blobResp.status}`}`);
              await routerExec("POST", "/agt/handoff/abort", {}, handoffHeaders).catch(() => {});
              process.exit(1);
            }

            // Send restore to target via port-forward
            const http = await import("node:http");
            const restorePayload = JSON.stringify({
              shared_secret: sharedSecret,
              blob: blobResp.body.blob,
            });

            const restoreResult: any = await new Promise((resolve, reject) => {
              const req = http.request(`http://127.0.0.1:${targetPort}/agt/handoff/restore`, {
                method: "POST",
                headers: {
                  "Content-Type": "application/json",
                  "Content-Length": Buffer.byteLength(restorePayload),
                },
                timeout: 30000,
              }, (res) => {
                let data = "";
                res.on("data", (c: Buffer) => { data += c.toString(); });
                res.on("end", () => {
                  try { resolve({ status: res.statusCode, body: JSON.parse(data) }); }
                  catch { resolve({ status: res.statusCode, body: { raw: data } }); }
                });
              });
              req.on("error", reject);
              req.write(restorePayload);
              req.end();
            });

            if (restoreResult.status >= 400) {
              stepper.fail(`Restore failed on target: ${restoreResult.body.error || `HTTP ${restoreResult.status}`}`);
              await routerExec("POST", "/agt/handoff/abort", {}, handoffHeaders).catch(() => {});
              process.exit(1);
            }

            stepper.done(`State transferred to AKS (${(snapshotSize / 1024).toFixed(1)} KB restored)`);

          } finally {
            // Clean up port-forward
            pfProc.kill();
          }

        } else {
          // ── aks_to_local: Full reverse handoff orchestration ────────────

          // Step 5a: Wake dormant local Docker container
          stepper.step("Waking dormant local agent...");
          const wakeResult = await wakeDormantDocker();
          if (!wakeResult.ready) {
            stepper.fail(wakeResult.error || "Failed to wake local container");
            await sourceExec("POST", "/agt/handoff/abort", {}, handoffHeaders).catch(() => {});
            aksPortForwardStop();
            process.exit(1);
          }
          stepper.done("Local agent running");

          // Step 5b: Re-hydrate credentials from K8s secret to Docker
          stepper.step("Re-hydrating credentials...");
          const injectedCreds = await rehydrateCredentials();
          if (injectedCreds.length > 0) {
            stepper.done(`Credentials injected: ${injectedCreds.join(", ")}`);
          } else {
            stepper.done("No cloud credentials to migrate (using local secrets)");
          }

          // Step 5c: Initialize handoff session on local router (needed for auth)
          stepper.step("Preparing local agent for restore...");
          const localAdminToken = await getAdminToken();
          if (!localAdminToken) {
            stepper.fail("Local admin token not found — cannot authenticate restore");
            await sourceExec("POST", "/agt/handoff/abort", {}, handoffHeaders).catch(() => {});
            aksPortForwardStop();
            process.exit(1);
          }
          const localAuthHeaders = { Authorization: `Bearer ${localAdminToken}` };
          const localInitResp = await routerExec("POST", "/agt/handoff/init", {
            direction: "aks_to_local",
            ttl_seconds: 300,
          }, localAuthHeaders);
          if (localInitResp.status >= 400) {
            stepper.fail(`Local init failed: ${localInitResp.body.error || `HTTP ${localInitResp.status}`}`);
            await sourceExec("POST", "/agt/handoff/abort", {}, handoffHeaders).catch(() => {});
            aksPortForwardStop();
            process.exit(1);
          }
          const localHandoffToken = localInitResp.body.handoff_token;
          const localHandoffHeaders = {
            ...localAuthHeaders,
            "X-Handoff-Token": localHandoffToken,
          };
          stepper.done("Local agent ready for restore");

          // Step 5d: Send the snapshot (already captured at step 3) to local Docker
          // Use stdin (@-) instead of -d arg to avoid shell argument length limits
          stepper.step("Restoring state to local agent...");
          const restorePayload = JSON.stringify({
            shared_secret: sharedSecret,
            blob: snapshotResp.body.blob,
          });
          const curlRestoreArgs = [
            "exec", "-i", containerName,
            "curl", "-sf", "--max-time", "60",
            "-X", "POST",
            "-H", "Content-Type: application/json",
            "-H", `Authorization: Bearer ${localAdminToken}`,
            "-H", `X-Handoff-Token: ${localHandoffToken}`,
            "-d", "@-",
            "-w", "\n%{http_code}",
            "http://127.0.0.1:8443/agt/handoff/restore",
          ];
          const curlRestore = await execa("docker", curlRestoreArgs, {
            input: restorePayload,
            stdio: ["pipe", "pipe", "pipe"],
          });
          const restoreLines = curlRestore.stdout.trimEnd().split("\n");
          const restoreStatus = parseInt(restoreLines[restoreLines.length - 1], 10);
          const restoreBodyRaw = restoreLines.slice(0, -1).join("\n");
          let localRestoreResp: { status: number; body: any };
          try {
            localRestoreResp = { status: restoreStatus, body: JSON.parse(restoreBodyRaw) };
          } catch {
            localRestoreResp = { status: restoreStatus, body: { raw: restoreBodyRaw } };
          }

          if (localRestoreResp.status >= 400) {
            stepper.fail(`Local restore failed: ${localRestoreResp.body.error || `HTTP ${localRestoreResp.status}`}`);
            await sourceExec("POST", "/agt/handoff/abort", {}, handoffHeaders).catch(() => {});
            aksPortForwardStop();
            process.exit(1);
          }

          stepper.done(`State restored to local (${(snapshotSize / 1024).toFixed(1)} KB)`);

          // Mark local handoff session as complete so the router is ready for future handoffs
          await routerExec("POST", "/agt/handoff/verify", {}, localHandoffHeaders).catch(() => {});
          await routerExec("POST", "/agt/handoff/decommission", {}, localHandoffHeaders).catch(() => {});
        }

        // Step 6: Succession (registry update)
        stepper.step("Registering identity succession...");

        // The source router signs the succession message with its private key
        // and submits directly to the registry. We just need the successor AMID.
        const sourceStatus = await sourceExec("GET", "/agt/status", undefined, authHeaders);
        const predecessorAmid = sourceStatus.body?.agent_did?.replace("did:agentmesh:", "") || "";

        if (predecessorAmid) {
          try {
            // Find successor AMID from registry (different from source)
            const regSearchResp = await sourceExec("GET",
              `/agt/registry/registry/search?capability=${encodeURIComponent(name)}`,
              undefined, authHeaders);
            const candidates = regSearchResp.body?.results?.filter(
              (a: any) => a.amid !== predecessorAmid && (a.display_name === name || a.capabilities?.includes(name))
            ) || [];

            if (candidates.length > 0) {
              const successorAmid = candidates[0].amid;
              // Let the source router sign and submit the succession
              const successionResp = await sourceExec("POST", "/agt/handoff/succession", {
                successor_amid: successorAmid,
                reason: `handoff:${direction}`,
              }, authHeaders);

              if (successionResp.status < 400) {
                stepper.done(`Identity succession: ${predecessorAmid.slice(0, 12)}... → ${successorAmid.slice(0, 12)}...`);
              } else {
                stepper.done(`Identity succession pending (${successionResp.body?.error || "registry returned error"})`);
              }
            } else {
              stepper.done("Identity succession pending (target AMID not yet registered)");
            }
          } catch {
            stepper.done("Identity succession pending (registry unreachable)");
          }
        } else {
          stepper.done("Identity succession ready (requires source agent AMID)");
        }

        // Step 7 (reverse only): Decommission cloud agent + scale down
        if (isReverse) {
          stepper.step("Decommissioning cloud agent...");
          try {
            await sourceExec("POST", "/agt/handoff/decommission", {}, handoffHeaders);
            stepper.done("Cloud agent decommissioned");
          } catch (decommErr: any) {
            stepper.done(`Decommission pending: ${decommErr.message}`);
          }

          // Delete parent + sub-agent CRDs. The controller will tear down the
          // namespaces, deployments, and services. A future forward handoff
          // creates everything fresh via `azureclaw up`.
          stepper.step("Destroying cloud sandboxes...");
          const allCrds = [name, ...subAgentNames];
          for (const crdName of allCrds) {
            try {
              await execa("kubectl", [
                "delete", "clawsandbox", crdName, "-n", "azureclaw-system",
                "--ignore-not-found",
              ], { stdio: "pipe", timeout: 10000 });
            } catch { /* best effort */ }
          }
          stepper.done(`Destroyed ${allCrds.length} cloud sandbox(es)`);

          aksPortForwardStop();
        }

        // Final step: Summary
        stepper.step("Handoff summary...");

        section("Handoff Result");
        kvLine("Direction", directionLabel);
        kvLine("Snapshot", `${(snapshotSize / 1024).toFixed(1)} KB`);
        if (snapshotItems.chat_messages) kvLine("  Messages", String(snapshotItems.chat_messages));
        if (snapshotItems.sub_agents) kvLine("  Sub-agents", String(snapshotItems.sub_agents));
        if (snapshotItems.trust_scores) kvLine("  Trust scores", String(snapshotItems.trust_scores));
        if (snapshotItems.audit_entries) kvLine("  Audit entries", String(snapshotItems.audit_entries));
        kvLine("Token hash", tokenHash?.slice(0, 16) || "—");

        console.log();
        console.log(chalk.green("  ✓ Handoff complete!"));
        console.log();

        // Send Telegram notification (best-effort)
        try {
          let tgToken: string | undefined;
          let tgChatId: string | undefined;
          if (direction === "aks_to_local") {
            // Credentials were just injected into the Docker container
            const { stdout: t } = await execa("docker", [
              "exec", containerName, "printenv", "TELEGRAM_BOT_TOKEN",
            ], { stdio: "pipe", reject: false });
            const { stdout: c } = await execa("docker", [
              "exec", containerName, "printenv", "TELEGRAM_ALLOW_FROM",
            ], { stdio: "pipe", reject: false });
            tgToken = t.trim() || undefined;
            tgChatId = c.trim() || undefined;
          } else {
            tgToken = process.env.TELEGRAM_BOT_TOKEN;
            tgChatId = process.env.TELEGRAM_ALLOW_FROM;
          }
          if (tgToken && tgChatId) {
            const label = direction === "local_to_aks"
              ? "☁️ I've moved to the cloud. Same me, new home — Azure AKS."
              : "🏠 I'm back on your local machine. Cloud instance decommissioned.";
            await fetch(
              `https://api.telegram.org/bot${tgToken}/sendMessage`,
              {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({ chat_id: tgChatId, text: label }),
              },
            );
          }
        } catch { /* best-effort — don't fail handoff for notification */ }

        if (direction === "local_to_aks") {
          console.log(chalk.dim("  Next steps:"));
          console.log(chalk.cyan(`    📡 Connect to cloud agent: azureclaw connect ${name}`));
          console.log(chalk.cyan(`    📊 Monitor agents:         azureclaw operator`));
          console.log();
          if (process.env.TELEGRAM_BOT_TOKEN) {
            console.log(chalk.dim(`    📱 Telegram: Your bot is now handled by the cloud agent.`));
          }
          console.log(chalk.dim(`    💤 Local agent is dormant (keys preserved). Reclaim: azureclaw handoff ${name} --to local`));
          console.log();
        } else {
          console.log(chalk.dim("  Your agent is back on local Docker."));
          console.log();
          console.log(chalk.dim("  Next steps:"));
          console.log(chalk.cyan(`    📡 Connect: azureclaw connect ${name} --local`));
          console.log(chalk.cyan(`    📊 Monitor: azureclaw operator`));
          console.log();
          if (process.env.TELEGRAM_BOT_TOKEN) {
            console.log(chalk.dim(`    📱 Telegram: Your bot is now handled by the local agent.`));
          }
          console.log(chalk.dim(`    ☁️  Cloud sandbox has been decommissioned.`));
          console.log();
        }

        stepper.done("Done");
        aksPortForwardStop();

      } catch (e: any) {
        aksPortForwardStop();
        console.log(chalk.red(`\n  Handoff failed: ${e.message}\n`));
        process.exit(1);
      }
    });

  return cmd;
}
