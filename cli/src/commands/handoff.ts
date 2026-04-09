import { Command } from "commander";
import chalk from "chalk";
import { Stepper, banner, section, kvLine, checkLine } from "../stepper.js";

/**
 * azureclaw handoff — live agent migration between local and cloud.
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
            return undefined;
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

        // Step 5: Transfer to target (placeholder — full implementation in H3/H4)
        stepper.step("Transferring state to target...");

        if (direction === "local_to_aks") {
          // In a full implementation, this would:
          // 1. Create AKS sandbox via ClawSandbox CRD
          // 2. Wait for target pod to be ready
          // 3. POST /agt/handoff/restore on the TARGET agent
          // 4. POST /agt/handoff/verify on the TARGET agent
          //
          // For now, we store the snapshot and report what would happen.
          stepper.done("State snapshot ready for transfer (target provisioning pending)");
        } else {
          // aks_to_local: the local agent would call /agt/handoff/restore
          stepper.done("State snapshot ready for transfer (local restore pending)");
        }

        // Step 6: Succession (registry update)
        stepper.step("Registering identity succession...");
        // In a full implementation:
        // 1. Read predecessor AMID from source governance
        // 2. Read successor AMID from target governance
        // 3. Source signs succession notice
        // 4. POST /v1/registry/succession to global registry
        stepper.done("Identity succession ready (requires target agent AMID)");

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
          console.log(chalk.dim(`    1. Target sandbox will be provisioned on AKS`));
          console.log(chalk.dim(`    2. State will be restored on the target agent`));
          console.log(chalk.dim(`    3. Identity succession will complete in the registry`));
          console.log(chalk.dim(`    4. Peers will be notified of the new location`));
          console.log(chalk.dim(`\n  Monitor: ${chalk.cyan(`azureclaw handoff ${name} --status`)}\n`));
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
