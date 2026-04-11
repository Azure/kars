import { Command } from "commander";
import chalk from "chalk";
import { Stepper, banner, section, kvLine, checkLine } from "../stepper.js";

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

      const stepper = new Stepper({ totalSteps: 7 });

      try {
        // Step 1: Verify source agent is running
        stepper.step("Verifying source agent...");
        const adminToken = await getAdminToken();
        if (!adminToken) {
          stepper.fail("Admin token not found — cannot initiate handoff");
          process.exit(1);
        }

        const authHeaders = { Authorization: `Bearer ${adminToken}` };

        // Check handoff status (also verifies connectivity + registry mode)
        const statusResp = await routerExec("GET", "/agt/handoff/status", undefined, authHeaders);
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
        const initResp = await routerExec("POST", "/agt/handoff/init", {
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

        const snapshotResp = await routerExec("POST", "/agt/handoff/snapshot", {
          shared_secret: sharedSecret,
        }, handoffHeaders);

        if (snapshotResp.status >= 400) {
          stepper.fail(`Snapshot failed: ${snapshotResp.body.error || `HTTP ${snapshotResp.status}`}`);
          // Try to abort
          await routerExec("POST", "/agt/handoff/abort", {}, handoffHeaders).catch(() => {});
          process.exit(1);
        }

        const snapshotSize = snapshotResp.body.snapshot_size_bytes || 0;
        const snapshotItems = snapshotResp.body.items || {};
        stepper.done(`Snapshot created (${(snapshotSize / 1024).toFixed(1)} KB)`);

        // Step 4: Drain — stop accepting new work
        stepper.step("Draining active work...");
        const drainResp = await routerExec("POST", "/agt/handoff/drain", {}, handoffHeaders);

        if (drainResp.status >= 400) {
          stepper.fail(`Drain failed: ${drainResp.body.error || `HTTP ${drainResp.status}`}`);
          await routerExec("POST", "/agt/handoff/abort", {}, handoffHeaders).catch(() => {});
          process.exit(1);
        }

        stepper.done("Source agent drained (no new work accepted)");

        // Step 5: Transfer to target
        stepper.step("Transferring state to target...");

        if (direction === "local_to_aks") {
          // ── H4: Provision target on AKS via ClawSandbox CRD ──────────────
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
            const blobResp = await routerExec("GET", "/agt/handoff/snapshot", undefined, handoffHeaders);
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
          // aks_to_local: the local agent would call /agt/handoff/restore
          stepper.done("State snapshot ready for transfer (local restore pending)");
        }

        // Step 6: Succession (registry update)
        stepper.step("Registering identity succession...");

        // Read AMIDs from source and target
        const sourceStatus = await routerExec("GET", "/agt/status", undefined, authHeaders);
        const predecessorAmid = sourceStatus.body?.agent_did?.replace("did:agentmesh:", "") || "";

        if (direction === "local_to_aks" && predecessorAmid) {
          // Read successor AMID from the target's registry entry
          try {
            const http = await import("node:http");
            const regSearchUrl = `http://127.0.0.1:8443/agt/registry/registry/search?capability=${encodeURIComponent(name)}`;
            const regResult: any = await new Promise((resolve, reject) => {
              const req = http.get(regSearchUrl, (res: any) => {
                let data = "";
                res.on("data", (c: Buffer) => { data += c.toString(); });
                res.on("end", () => {
                  try { resolve(JSON.parse(data)); } catch { resolve(null); }
                });
              });
              req.on("error", reject);
              req.setTimeout(5000, () => { req.destroy(); reject(new Error("timeout")); });
            });

            // Find the AKS agent's AMID (different from our local AMID)
            const candidates = regResult?.results?.filter(
              (a: any) => a.amid !== predecessorAmid && (a.display_name === name || a.capabilities?.includes(name))
            ) || [];

            if (candidates.length > 0) {
              const successorAmid = candidates[0].amid;
              // POST succession to the global registry
              const successionResp = await routerExec("POST", "/agt/registry/registry/succession", {
                predecessor_amid: predecessorAmid,
                successor_amid: successorAmid,
                reason: "handoff:local_to_aks",
              }, authHeaders);

              if (successionResp.status < 400) {
                stepper.done(`Identity succession: ${predecessorAmid.slice(0, 12)}... → ${successorAmid.slice(0, 12)}...`);
              } else {
                stepper.done(`Identity succession pending (${successionResp.body.error || "registry returned error"})`);
              }
            } else {
              stepper.done("Identity succession pending (target AMID not yet registered)");
            }
          } catch {
            stepper.done("Identity succession pending (registry unreachable)");
          }
        } else {
          stepper.done("Identity succession ready (requires target agent AMID)");
        }

        // Step 7: Summary
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
        console.log(chalk.green("  ✓ Handoff initiated successfully."));
        console.log();

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
          console.log(chalk.dim("  Next steps:"));
          console.log(chalk.dim(`    1. Local agent will restore the state snapshot`));
          console.log(chalk.dim(`    2. Sub-agents will be re-spawned as Docker containers`));
          console.log(chalk.dim(`    3. Co-signed reclamation will update the registry`));
          console.log(chalk.dim(`    4. Cloud agent will decommission\n`));
        }

      } catch (e: any) {
        console.log(chalk.red(`\n  Handoff failed: ${e.message}\n`));
        process.exit(1);
      }
    });

  return cmd;
}
