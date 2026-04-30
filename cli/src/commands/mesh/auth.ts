// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Phase 2 / S15.b: `azureclaw mesh auth` subcommand body extracted
// from mesh.ts. Attaches as a subcommand of an existing Commander command.

import chalk from "chalk";
import { Command } from "commander";
import * as crypto from "node:crypto";
import { banner, section, kvLine, checkLine } from "../../stepper.js";
import {
  IDENTITY_FILE,
  generateKeypair,
  encryptPrivateKey,
  decryptPrivateKey,
  loadIdentity,
  saveIdentity,
} from "./identity.js";
import { sanitizeForLog, waitForOAuthCallback } from "./oauth.js";

export function attachAuthSubcommand(cmd: Command): void {
  cmd
    .command("auth")
    .description("Authenticate with an AgentMesh registry via OAuth")
    .requiredOption(
      "--registry <url>",
      "Registry URL (e.g. https://registry.example.com)"
    )
    .option("--provider <provider>", "OAuth provider (github, entra)", "github")
    .option("--no-browser", "Print URL instead of opening browser")
    .action(async (opts: { registry: string; provider: string; browser: boolean }) => {
      banner("AzureClaw · Mesh Auth", "AgentMesh Identity & Registration");

      const registryUrl = opts.registry.replace(/\/+$/, "");
      const provider = opts.provider.toLowerCase();

      if (!["github", "entra", "google"].includes(provider)) {
        console.error(
          chalk.red(`  ✘ Unknown provider: ${provider}. Use github, entra, or google.`)
        );
        process.exit(1);
      }

      // Step 1: Check existing identity
      section("Identity");
      let identity = loadIdentity();
      let amid: string;
      let publicKeyB64: string;

      if (identity) {
        amid = identity.amid;
        publicKeyB64 = identity.publicKey;
        kvLine("Existing AMID", amid);
        kvLine("Created", identity.createdAt);
        if (identity.provider) {
          kvLine("Verified via", `${identity.provider} (${identity.email ?? identity.username ?? "—"})`);
        }
      } else {
        console.log(chalk.dim("  Generating new Ed25519 keypair..."));
        const kp = generateKeypair();
        publicKeyB64 = kp.publicKey.toString("base64");
        amid = kp.amid;

        const enc = encryptPrivateKey(kp.privateKey);
        identity = {
          amid,
          publicKey: publicKeyB64,
          encryptedPrivateKey: enc.encrypted,
          iv: enc.iv,
          authTag: enc.authTag,
          createdAt: new Date().toISOString(),
        };
        saveIdentity(identity);
        kvLine("New AMID", amid);
        checkLine(true, `Keypair saved to ${IDENTITY_FILE}`);
      }

      // Step 2: Check registry providers
      section("Registry");
      kvLine("URL", registryUrl);

      let providers: Array<{ name: string; enabled: boolean; display_name: string }>;
      try {
        const resp = await fetch(`${registryUrl}/v1/auth/oauth/providers`);
        if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
        const data = (await resp.json()) as { providers: typeof providers };
        providers = data.providers;
      } catch (e: any) {
        console.error(chalk.red(`  ✘ Cannot reach registry: ${e.message}`));
        process.exit(1);
      }

      const selected = providers.find((p) => p.name === provider);
      if (!selected || !selected.enabled) {
        console.error(
          chalk.red(
            `  ✘ Provider "${provider}" is not enabled on this registry.`
          )
        );
        const enabled = providers
          .filter((p) => p.enabled)
          .map((p) => p.name);
        if (enabled.length > 0) {
          console.log(
            chalk.dim(`  Available: ${enabled.join(", ")}`)
          );
        }
        process.exit(1);
      }

      checkLine(true, `Provider ${selected.display_name} enabled`);

      // Step 3: Start OAuth flow
      section("OAuth Flow");

      // Find a free port for the callback
      const callbackPort = 19876 + Math.floor(Math.random() * 100);
      const timestamp = new Date().toISOString();

      // Sign the timestamp to prove AMID ownership
      const privateKeyBuf = decryptPrivateKey(identity);
      const privateKeyObj = crypto.createPrivateKey({
        key: Buffer.concat([
          // Wrap raw 32-byte key in PKCS8 DER envelope for Ed25519
          Buffer.from(
            "302e020100300506032b657004220420",
            "hex"
          ),
          privateKeyBuf,
        ]),
        format: "der",
        type: "pkcs8",
      });
      const signature = crypto.sign(null, Buffer.from(timestamp), privateKeyObj);
      const signatureB64 = signature.toString("base64");

      // Call authorize endpoint
      let authUrl: string;
      try {
        const resp = await fetch(`${registryUrl}/v1/auth/oauth/authorize`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            amid,
            provider,
            signature: signatureB64,
            timestamp,
          }),
        });
        if (!resp.ok) {
          const err = await resp.json().catch(() => ({ error: "Unknown error" })) as { error: string };
          throw new Error(err.error || `HTTP ${resp.status}`);
        }
        const data = (await resp.json()) as { authorization_url: string };
        authUrl = data.authorization_url;
      } catch (e: any) {
        console.error(chalk.red(`  ✘ Failed to start OAuth flow: ${e.message}`));
        process.exit(1);
      }

      if (opts.browser) {
        console.log(chalk.dim("  Opening browser for authentication..."));
        const open = await import("open").catch(() => null);
        if (open) {
          await open.default(authUrl);
        } else {
          console.log(
            chalk.yellow("  Could not open browser. Visit this URL:")
          );
          console.log(`  ${chalk.cyan(authUrl)}\n`);
        }
      } else {
        console.log(chalk.dim("  Visit this URL to authenticate:"));
        console.log(`  ${chalk.cyan(authUrl)}\n`);
      }

      console.log(chalk.dim("  Waiting for OAuth callback..."));

      // Step 4: Wait for callback
      try {
        const result = await waitForOAuthCallback(callbackPort);

        if (result.success && result.verified_identity) {
          section("Verified");
          checkLine(true, `Provider: ${result.verified_identity.provider}`);
          if (result.verified_identity.email) {
            kvLine("Email", result.verified_identity.email);
          }
          if (result.verified_identity.username) {
            kvLine("Username", result.verified_identity.username);
          }

          // Update identity with verification info
          identity.provider = result.provider;
          identity.email = result.verified_identity.email ?? undefined;
          identity.username = result.verified_identity.username ?? undefined;
          identity.verifiedAt = new Date().toISOString();
          identity.registryUrl = registryUrl;
          saveIdentity(identity);

          checkLine(true, `Identity updated: ${IDENTITY_FILE}`);
          console.log();
          console.log(
            chalk.green("  ✓ ") +
              chalk.bold("Mesh identity verified and registered.")
          );
          console.log(
            chalk.dim(
              `    Use ${chalk.cyan(
                `azureclaw dev --global-registry ${registryUrl}`
              )} to connect agents.`
            )
          );
        } else {
          console.error(
            chalk.red(`  ✘ Verification failed: ${sanitizeForLog(result.error ?? "Unknown error")}`)
          );
          process.exit(1);
        }
      } catch (e: any) {
        console.error(chalk.red(`  ✘ ${e.message}`));
        process.exit(1);
      }
    });
}
