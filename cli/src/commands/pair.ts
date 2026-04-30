// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import chalk from "chalk";
import * as crypto from "node:crypto";
import { execa } from "execa";
import { banner, section, kvLine, checkLine } from "../stepper.js";

// ---------------------------------------------------------------------------
// Token format: azcp_1_<base64url(JSON payload)>
// Payload: { controller_amid, relay_url, registry_url, secret }
// ---------------------------------------------------------------------------

const TOKEN_PREFIX = "azcp_1_";
const PAIRING_NAMESPACE = "azureclaw-system";

interface PairingTokenPayload {
  controller_amid: string;
  relay_url: string;
  registry_url: string;
  secret: string;
}

/** Encode a pairing token from its components. */
function encodeToken(payload: PairingTokenPayload): string {
  const json = JSON.stringify(payload);
  const b64 = Buffer.from(json).toString("base64url");
  return `${TOKEN_PREFIX}${b64}`;
}

/** Decode a pairing token. Returns null if invalid. */
export function decodeToken(token: string): PairingTokenPayload | null {
  if (!token.startsWith(TOKEN_PREFIX)) return null;
  try {
    const b64 = token.slice(TOKEN_PREFIX.length);
    const json = Buffer.from(b64, "base64url").toString("utf-8");
    const payload = JSON.parse(json);
    if (!payload.controller_amid || !payload.secret) return null;
    return payload as PairingTokenPayload;
  } catch {
    return null;
  }
}

/** SHA-256 hash of a string, hex-encoded. */
function sha256hex(input: string): string {
  return crypto.createHash("sha256").update(input).digest("hex");
}

// ---------------------------------------------------------------------------
// CLI command
// ---------------------------------------------------------------------------

export function pairCommand(): Command {
  const cmd = new Command("pair");
  cmd.description("Manage federation pairings for external agent cloud offload");

  // ── azureclaw pair generate ──
  cmd
    .command("generate")
    .description("Generate a one-time pairing token for an external agent")
    .requiredOption("--name <name>", "Display name for this pairing (e.g., alice-laptop)")
    .option("--expires <duration>", "Expiry duration (e.g., 90d, 30d, 7d)", "90d")
    .option("--token-budget <tokens>", "Maximum tokens for offloads", "500000")
    .option("--slots <n>", "Maximum concurrent offload sandboxes", "1")
    .option("--capabilities <list>", "Comma-separated: offload,handoff", "offload,handoff")
    .option("--relay-url <url>", "AgentMesh relay URL", "ws://host.docker.internal:18765")
    .option("--registry-url <url>", "AgentMesh registry URL", "http://host.docker.internal:18080")
    .action(async (options) => {
      banner("AzureClaw · Pair", "Federation Pairing Token");

      // Parse expiry duration
      const expiresAt = parseDuration(options.expires);
      if (!expiresAt) {
        console.error(chalk.red(`Invalid duration: ${options.expires} (use e.g., 90d, 30d, 7d, 24h)`));
        process.exit(1);
      }

      // Get controller AMID from cluster
      section("Reading controller identity...");
      let controllerAmid: string;
      try {
        const { stdout } = await execa("kubectl", [
          "get", "secret", "controller-mesh-identity",
          "-n", PAIRING_NAMESPACE,
          "-o", "jsonpath={.data.amid}",
        ], { stdio: "pipe" });
        controllerAmid = Buffer.from(stdout, "base64").toString("utf-8");
        if (!controllerAmid) throw new Error("empty");
      } catch {
        // If no mesh identity yet, generate a placeholder AMID
        // The controller will create its real identity on startup
        controllerAmid = `ctrl_${crypto.randomBytes(10).toString("hex")}`;
        console.log(chalk.dim("  Controller mesh identity not found — using generated AMID"));
        console.log(chalk.dim("  (Controller will bind its real AMID once it starts and joins the relay)"));
      }

      // Generate the pairing secret (256-bit random)
      const secret = crypto.randomBytes(32).toString("base64url");
      const tokenHash = sha256hex(secret);

      // Encode the full token
      const token = encodeToken({
        controller_amid: controllerAmid,
        relay_url: options.relayUrl,
        registry_url: options.registryUrl,
        secret,
      });

      // Parse capabilities
      const capabilities = options.capabilities.split(",").map((c: string) => c.trim());

      // Build ClawPairing CRD
      const pairing = {
        apiVersion: "azureclaw.azure.com/v1alpha1",
        kind: "ClawPairing",
        metadata: {
          name: sanitizeName(options.name),
          namespace: PAIRING_NAMESPACE,
        },
        spec: {
          tokenHash: tokenHash,
          expiresAt: expiresAt.toISOString(),
          slotsMax: parseInt(options.slots, 10),
          tokenBudget: parseInt(options.tokenBudget, 10),
          capabilities,
          displayName: options.name,
        },
      };

      // Apply to cluster
      section("Creating pairing...");
      try {
        await execa("kubectl", ["apply", "-f", "-"], {
          input: JSON.stringify(pairing),
          stdio: ["pipe", "pipe", "pipe"],
        });
        checkLine(true, "ClawPairing created");
      } catch (err: any) {
        console.error(chalk.red(`Failed to create pairing: ${err.stderr || err.message}`));
        process.exit(1);
      }

      // Print the token
      const budgetStr = parseInt(options.tokenBudget, 10).toLocaleString();
      console.log();
      console.log(chalk.green("  ╭──────────────────────────────────────────────────────────────╮"));
      console.log(chalk.green("  │") + chalk.bold("  🔑 Pairing Token Generated") + chalk.green("                                  │"));
      console.log(chalk.green("  │") + chalk.green("                                                              │"));
      kvLine("  Name", options.name);
      kvLine("  Expires", expiresAt.toLocaleDateString("en-US", { year: "numeric", month: "long", day: "numeric" }));
      kvLine("  Budget", `${budgetStr} tokens`);
      kvLine("  Slots", `${options.slots} concurrent sandbox${options.slots === "1" ? "" : "es"}`);
      kvLine("  Capabilities", capabilities.join(", "));
      console.log(chalk.green("  │") + chalk.green("                                                              │"));
      console.log(chalk.green("  │") + "  Token:" + chalk.green("                                                      │"));
      // Print token in chunks for readability
      const tokenStr = token;
      const chunkSize = 58;
      for (let i = 0; i < tokenStr.length; i += chunkSize) {
        const chunk = tokenStr.slice(i, i + chunkSize);
        console.log(chalk.green("  │") + `  ${chalk.cyan(chunk)}`.padEnd(65) + chalk.green("│"));
      }
      console.log(chalk.green("  │") + chalk.green("                                                              │"));
      console.log(chalk.green("  │") + chalk.yellow("  ⚠  One-time use. Share securely (Signal, encrypted email).") + chalk.green(" │"));
      console.log(chalk.green("  ╰──────────────────────────────────────────────────────────────╯"));
      console.log();
      // Print raw token for easy copying
      console.log(chalk.dim("  Token (single line):"));
      console.log(`  ${token}`);
      console.log();
      // Try to copy to clipboard
      try {
        const { execSync } = await import("child_process");
        if (process.platform === "darwin") {
          execSync("pbcopy", { input: token });
          console.log(chalk.green("  ✓ Copied to clipboard"));
        } else if (process.platform === "linux") {
          execSync("xclip -selection clipboard", { input: token });
          console.log(chalk.green("  ✓ Copied to clipboard"));
        }
      } catch {
        // clipboard not available — no problem
      }
      console.log();
    });

  // ── azureclaw pair list ──
  cmd
    .command("list")
    .description("List all federation pairings")
    .action(async () => {
      banner("AzureClaw · Pairings", "Federation Status");

      try {
        const { stdout } = await execa("kubectl", [
          "get", "clawpairings", "-n", PAIRING_NAMESPACE,
          "-o", "json",
        ], { stdio: "pipe" });

        const result = JSON.parse(stdout);
        const items = result.items || [];

        if (items.length === 0) {
          console.log(chalk.dim("  No pairings found. Create one with: azureclaw pair generate --name <name>"));
          return;
        }

        // Table header
        console.log(
          chalk.bold("  Name".padEnd(22)) +
          chalk.bold("Status".padEnd(18)) +
          chalk.bold("AMID".padEnd(24)) +
          chalk.bold("Offloads".padEnd(12)) +
          chalk.bold("Tokens Used".padEnd(16)) +
          chalk.bold("Expires")
        );
        console.log(chalk.dim("  " + "─".repeat(98)));

        for (const item of items) {
          const name = (item.metadata?.name || "").padEnd(20);
          const phase = item.status?.phase || "Unknown";
          const phaseColor = phase === "Active" ? chalk.green : phase === "PendingPairing" ? chalk.yellow : chalk.red;
          const amid = (item.status?.boundAmid || "—").slice(0, 20).padEnd(22);
          const offloads = String(item.status?.offloadsCompleted || 0).padEnd(10);
          const tokensUsed = String(item.status?.tokensUsed || 0).padEnd(14);
          const expires = item.spec?.expiresAt ? new Date(item.spec.expiresAt).toLocaleDateString() : "—";

          console.log(
            `  ${chalk.cyan(name)}` +
            `${phaseColor(phase.padEnd(16))}` +
            `${chalk.dim(amid)}` +
            `${offloads}` +
            `${tokensUsed}` +
            `${expires}`
          );
        }
        console.log();
      } catch (err: any) {
        if (err.stderr?.includes("not found") || err.stderr?.includes("no matches")) {
          console.log(chalk.dim("  ClawPairing CRD not installed. Deploy with Helm first."));
        } else {
          console.error(chalk.red(`  Failed to list pairings: ${err.stderr || err.message}`));
        }
      }
    });

  // ── azureclaw pair revoke ──
  cmd
    .command("revoke")
    .description("Revoke a pairing (blocks future offloads)")
    .argument("<name>", "Name of the pairing to revoke")
    .action(async (name: string) => {
      banner("AzureClaw · Revoke Pairing", "");

      const safeName = sanitizeName(name);

      try {
        // Patch status to Revoked
        const patch = JSON.stringify({
          status: { phase: "Revoked" },
        });
        await execa("kubectl", [
          "patch", "clawpairing", safeName,
          "-n", PAIRING_NAMESPACE,
          "--type=merge", "--subresource=status",
          "-p", patch,
        ], { stdio: "pipe" });

        checkLine(true, `Pairing '${safeName}' revoked`);
        console.log(chalk.dim("  Active offloads will complete, but no new offloads will be accepted."));
      } catch (err: any) {
        console.error(chalk.red(`  Failed to revoke: ${err.stderr || err.message}`));
        process.exit(1);
      }
    });

  // ── azureclaw pair inspect ──
  cmd
    .command("inspect")
    .description("Show detailed info about a pairing")
    .argument("<name>", "Name of the pairing to inspect")
    .action(async (name: string) => {
      banner("AzureClaw · Pairing Details", "");

      const safeName = sanitizeName(name);

      try {
        const { stdout } = await execa("kubectl", [
          "get", "clawpairing", safeName,
          "-n", PAIRING_NAMESPACE,
          "-o", "json",
        ], { stdio: "pipe" });

        const item = JSON.parse(stdout);
        const spec = item.spec || {};
        const status = item.status || {};

        kvLine("  Name", safeName);
        kvLine("  Display Name", spec.displayName || "—");

        const phase = status.phase || "Unknown";
        const phaseColor = phase === "Active" ? chalk.green : phase === "PendingPairing" ? chalk.yellow : chalk.red;
        kvLine("  Status", phaseColor(phase));

        kvLine("  Bound AMID", status.boundAmid || chalk.dim("(not yet paired)"));
        kvLine("  Paired at", status.pairedAt ? new Date(status.pairedAt).toLocaleString() : "—");
        kvLine("  Capabilities", (spec.capabilities || []).join(", "));

        console.log();
        section("Usage");
        kvLine("  Offloads completed", String(status.offloadsCompleted || 0));
        kvLine("  Offloads failed", String(status.offloadsFailed || 0));
        kvLine("  Last offload", status.lastOffloadAt ? new Date(status.lastOffloadAt).toLocaleString() : "—");
        kvLine("  Active sandbox", status.activeSandbox || "none");

        console.log();
        section("Budget");
        const used = status.tokensUsed || 0;
        const budget = spec.tokenBudget || 500000;
        const pct = budget > 0 ? Math.round((used / budget) * 100) : 0;
        kvLine("  Tokens used", `${used.toLocaleString()} / ${budget.toLocaleString()} (${pct}%)`);
        kvLine("  Slots", `${status.slotsUsed || 0} / ${spec.slotsMax || 1} active`);

        console.log();
        section("Expiry");
        kvLine("  Expires at", spec.expiresAt ? new Date(spec.expiresAt).toLocaleString() : "—");
        if (spec.expiresAt) {
          const expiry = new Date(spec.expiresAt);
          const now = new Date();
          const daysLeft = Math.ceil((expiry.getTime() - now.getTime()) / (1000 * 60 * 60 * 24));
          kvLine("  Days remaining", daysLeft > 0 ? `${daysLeft}` : chalk.red("EXPIRED"));
        }
        console.log();
      } catch (err: any) {
        console.error(chalk.red(`  Pairing '${safeName}' not found: ${err.stderr || err.message}`));
        process.exit(1);
      }
    });

  return cmd;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Parse a duration string like "90d", "30d", "7d", "24h" into a Date. */
function parseDuration(input: string): Date | null {
  const match = input.match(/^(\d+)(d|h|m)$/);
  if (!match) return null;

  const value = parseInt(match[1], 10);
  const unit = match[2];
  const now = new Date();

  switch (unit) {
    case "d":
      now.setDate(now.getDate() + value);
      break;
    case "h":
      now.setHours(now.getHours() + value);
      break;
    case "m":
      now.setMinutes(now.getMinutes() + value);
      break;
    default:
      return null;
  }
  return now;
}

/** Sanitize a name to be K8s-safe (lowercase, alphanumeric + hyphens). */
function sanitizeName(name: string): string {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9-]/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-|-$/g, "")
    .slice(0, 63);
}
