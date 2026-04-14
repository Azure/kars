import { Command } from "commander";
import chalk from "chalk";
import * as fs from "node:fs";
import * as path from "node:path";
import * as os from "node:os";
import * as http from "node:http";
import * as crypto from "node:crypto";
import { execa } from "execa";
import { spawn } from "node:child_process";
import * as net from "node:net";
import { banner, section, kvLine, checkLine } from "../stepper.js";
import { loadContext, saveContext } from "../config.js";

// ---------------------------------------------------------------------------
// Identity file
// ---------------------------------------------------------------------------

const IDENTITY_DIR = path.join(os.homedir(), ".azureclaw");
const IDENTITY_FILE = path.join(IDENTITY_DIR, "mesh-identity.json");

export interface MeshIdentity {
  amid: string;
  publicKey: string;
  /** Encrypted private key (AES-256-GCM, key derived from machine ID) */
  encryptedPrivateKey: string;
  /** Initialization vector for AES-GCM */
  iv: string;
  /** Auth tag for AES-GCM */
  authTag: string;
  provider?: string;
  email?: string;
  username?: string;
  verifiedAt?: string;
  registryUrl?: string;
  createdAt: string;
}

// ---------------------------------------------------------------------------
// Encryption helpers for at-rest key protection
// ---------------------------------------------------------------------------

/** Derive an encryption key from a stable machine-specific seed. */
function deriveEncryptionKey(): Buffer {
  // Use a combination of hostname + homedir as a machine-bound seed.
  // This isn't HSM-grade but protects against casual file theft.
  const seed = `azureclaw:mesh-identity:${os.hostname()}:${os.homedir()}`;
  return crypto.createHash("sha256").update(seed).digest();
}

function encryptPrivateKey(privateKey: Buffer): {
  encrypted: string;
  iv: string;
  authTag: string;
} {
  const key = deriveEncryptionKey();
  const iv = crypto.randomBytes(12);
  const cipher = crypto.createCipheriv("aes-256-gcm", key, iv);
  const encrypted = Buffer.concat([
    cipher.update(privateKey),
    cipher.final(),
  ]);
  const authTag = cipher.getAuthTag();
  return {
    encrypted: encrypted.toString("base64"),
    iv: iv.toString("base64"),
    authTag: authTag.toString("base64"),
  };
}

function decryptPrivateKey(identity: MeshIdentity): Buffer {
  const key = deriveEncryptionKey();
  const iv = Buffer.from(identity.iv, "base64");
  const authTag = Buffer.from(identity.authTag, "base64");
  const encrypted = Buffer.from(identity.encryptedPrivateKey, "base64");
  const decipher = crypto.createDecipheriv("aes-256-gcm", key, iv);
  decipher.setAuthTag(authTag);
  return Buffer.concat([decipher.update(encrypted), decipher.final()]);
}

// ---------------------------------------------------------------------------
// Ed25519 key generation + AMID derivation
// ---------------------------------------------------------------------------

function generateKeypair(): {
  publicKey: Buffer;
  privateKey: Buffer;
  amid: string;
} {
  const { publicKey, privateKey } = crypto.generateKeyPairSync("ed25519", {
    publicKeyEncoding: { type: "spki", format: "der" },
    privateKeyEncoding: { type: "pkcs8", format: "der" },
  });

  // Extract raw 32-byte keys from DER encoding
  // Ed25519 SPKI: last 32 bytes are the raw public key
  const rawPub = publicKey.subarray(publicKey.length - 32);
  // Ed25519 PKCS8: last 32 bytes are the raw private key
  const rawPriv = privateKey.subarray(privateKey.length - 32);

  // AMID = base58(sha256(publicKey)[:20])
  const hash = crypto.createHash("sha256").update(rawPub).digest();
  const amid = base58Encode(hash.subarray(0, 20));

  return { publicKey: rawPub, privateKey: rawPriv, amid };
}

// Minimal base58 encoder (Bitcoin alphabet)
const BASE58_ALPHABET =
  "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

function base58Encode(buffer: Buffer): string {
  let num = BigInt("0x" + buffer.toString("hex"));
  const chars: string[] = [];
  while (num > 0n) {
    chars.unshift(BASE58_ALPHABET[Number(num % 58n)]);
    num = num / 58n;
  }
  // Preserve leading zeros
  for (const byte of buffer) {
    if (byte === 0) chars.unshift("1");
    else break;
  }
  return chars.join("");
}

// ---------------------------------------------------------------------------
// Identity loading / saving
// ---------------------------------------------------------------------------

function loadIdentity(): MeshIdentity | null {
  if (!fs.existsSync(IDENTITY_FILE)) return null;
  try {
    const data = JSON.parse(fs.readFileSync(IDENTITY_FILE, "utf-8"));
    return data as MeshIdentity;
  } catch {
    return null;
  }
}

function saveIdentity(identity: MeshIdentity): void {
  fs.mkdirSync(IDENTITY_DIR, { recursive: true, mode: 0o700 });
  fs.writeFileSync(IDENTITY_FILE, JSON.stringify(identity, null, 2), {
    mode: 0o600,
  });
}

// ---------------------------------------------------------------------------
// OAuth callback server
// ---------------------------------------------------------------------------

interface OAuthResult {
  success: boolean;
  amid: string;
  provider: string;
  verified_identity?: {
    provider: string;
    provider_id: string;
    email?: string;
    username?: string;
    display_name?: string;
  };
  certificate?: string;
  error?: string;
}

async function waitForOAuthCallback(
  port: number,
  timeoutMs: number = 300_000
): Promise<OAuthResult> {
  return new Promise((resolve, reject) => {
    const server = http.createServer((req, res) => {
      const url = new URL(req.url ?? "/", `http://localhost:${port}`);

      if (url.pathname === "/callback") {
        // The registry redirects here with the verification result as query params
        const resultJson = url.searchParams.get("result");
        if (resultJson) {
          try {
            const result = JSON.parse(
              Buffer.from(resultJson, "base64").toString("utf-8")
            ) as OAuthResult;

            // Return a nice HTML page
            res.writeHead(200, { "Content-Type": "text/html" });
            res.end(`
              <html><body style="font-family: system-ui; text-align: center; padding-top: 80px;">
                <h2>${result.success ? "✅ Authenticated!" : "❌ Authentication failed"}</h2>
                <p>${result.success ? "You can close this tab and return to the terminal." : result.error ?? "Unknown error"}</p>
              </body></html>
            `);

            server.close();
            resolve(result);
          } catch {
            res.writeHead(400, { "Content-Type": "text/plain" });
            res.end("Invalid callback data");
          }
        } else {
          res.writeHead(400, { "Content-Type": "text/plain" });
          res.end("Missing result parameter");
        }
      } else {
        res.writeHead(404);
        res.end();
      }
    });

    server.listen(port, "127.0.0.1");

    const timer = setTimeout(() => {
      server.close();
      reject(new Error("OAuth callback timed out after 5 minutes"));
    }, timeoutMs);

    server.on("close", () => clearTimeout(timer));
  });
}

// ---------------------------------------------------------------------------
// Command implementation
// ---------------------------------------------------------------------------

export function meshCommand(): Command {
  const cmd = new Command("mesh");
  cmd.description(
    "Manage AgentMesh identity and authentication for cross-environment handoff"
  );

  // -----------------------------------------------------------------------
  // mesh auth
  // -----------------------------------------------------------------------
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
            chalk.red(`  ✘ Verification failed: ${result.error ?? "Unknown error"}`)
          );
          process.exit(1);
        }
      } catch (e: any) {
        console.error(chalk.red(`  ✘ ${e.message}`));
        process.exit(1);
      }
    });

  // -----------------------------------------------------------------------
  // mesh status
  // -----------------------------------------------------------------------
  cmd
    .command("status")
    .description("Show current mesh identity")
    .action(async () => {
      banner("AzureClaw · Mesh Identity", "AgentMesh Identity Status");

      const identity = loadIdentity();
      if (!identity) {
        console.log(chalk.dim("  No mesh identity found."));
        console.log(
          chalk.dim(
            `  Run ${chalk.cyan("azureclaw mesh auth --registry <url>")} to create one.`
          )
        );
        return;
      }

      kvLine("AMID", identity.amid);
      kvLine("Public Key", identity.publicKey.substring(0, 20) + "...");
      kvLine("Created", identity.createdAt);

      if (identity.provider) {
        kvLine("Provider", identity.provider);
        if (identity.email) kvLine("Email", identity.email);
        if (identity.username) kvLine("Username", identity.username);
        if (identity.verifiedAt) kvLine("Verified", identity.verifiedAt);
      } else {
        console.log(chalk.yellow("  ⚠ Not verified (anonymous)"));
      }

      if (identity.registryUrl) {
        kvLine("Registry", identity.registryUrl);
      }

      console.log(
        chalk.dim(`\n  Identity file: ${IDENTITY_FILE}`)
      );
    });

  // -----------------------------------------------------------------------
  // mesh reset
  // -----------------------------------------------------------------------
  cmd
    .command("reset")
    .description("Delete mesh identity (requires re-authentication)")
    .action(async () => {
      if (!fs.existsSync(IDENTITY_FILE)) {
        console.log(chalk.dim("  No mesh identity to reset."));
        return;
      }

      const { default: inquirer } = await import("inquirer");
      const { confirm } = await inquirer.prompt([
        {
          type: "confirm",
          name: "confirm",
          message:
            "This will delete your mesh identity. You will need to re-authenticate. Continue?",
          default: false,
        },
      ]);

      if (confirm) {
        fs.unlinkSync(IDENTITY_FILE);
        checkLine(true, `Identity deleted: ${IDENTITY_FILE}`);
      } else {
        console.log(chalk.dim("  Cancelled."));
      }
    });

  // -----------------------------------------------------------------------
  // mesh promote — expose cluster registry as a global endpoint
  // -----------------------------------------------------------------------
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

        // ── Health check: registry ──
        let registryHealthy = false;
        try {
          const resp = await fetch(`http://localhost:${regPort}/v1/health`, {
            signal: AbortSignal.timeout(3000),
          });
          if (resp.ok) {
            const body = await resp.json() as Record<string, unknown>;
            checkLine(true, `Registry healthy (${body.agents_registered ?? 0} agents, ${body.agents_online ?? 0} online)`);
            registryHealthy = true;
          } else {
            checkLine(false, `Registry returned HTTP ${resp.status}`);
          }
        } catch {
          checkLine(false, "Registry not reachable");
        }

        // ── Health check: relay ──
        let relayHealthy = false;
        try {
          // Relay is a WebSocket server — TCP connect is enough
          await new Promise<void>((resolve, reject) => {
            const sock = net.connect(relayPort, "127.0.0.1", () => { sock.destroy(); resolve(); });
            sock.on("error", reject);
            sock.setTimeout(3000, () => { sock.destroy(); reject(new Error("timeout")); });
          });
          checkLine(true, `Relay listening on localhost:${relayPort}`);
          relayHealthy = true;
        } catch {
          checkLine(false, "Relay not reachable");
        }

        if (registryHealthy && relayHealthy) {
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

          // Kill anything still holding the ports
          for (const port of [regPort, relayPort]) {
            try {
              const { stdout } = await execa("lsof", ["-ti", `:${port}`], { stdio: "pipe", reject: false });
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
              "-n", "agentmesh",
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

          // Final health check
          section("Connectivity Check");
          try {
            const resp = await fetch(`http://localhost:${regPort}/v1/health`, {
              signal: AbortSignal.timeout(5000),
            });
            if (resp.ok) {
              const body = await resp.json() as Record<string, unknown>;
              checkLine(true, `Registry healthy (${body.agents_registered ?? 0} agents registered)`);
            } else {
              checkLine(false, `Registry returned ${resp.status}`);
            }
          } catch (e: any) {
            checkLine(false, `Registry not reachable: ${e.message}`);
          }

          try {
            await new Promise<void>((resolve, reject) => {
              const sock = net.connect(relayPort, "127.0.0.1", () => { sock.destroy(); resolve(); });
              sock.on("error", reject);
              sock.setTimeout(3000, () => { sock.destroy(); reject(new Error("timeout")); });
            });
            checkLine(true, `Relay listening on localhost:${relayPort}`);
          } catch (e: any) {
            checkLine(false, `Relay not reachable: ${e.message}`);
          }

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
            "-n", "agentmesh",
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

        // Verify connectivity
        section("Connectivity Check");
        try {
          const resp = await fetch(`http://localhost:${regPort}/v1/health`, {
            signal: AbortSignal.timeout(5000),
          });
          if (resp.ok) {
            const body = await resp.json() as Record<string, unknown>;
            checkLine(true, `Registry healthy (${body.agents_registered ?? 0} agents registered)`);
          } else {
            checkLine(false, `Registry returned ${resp.status}`);
          }
        } catch (e: any) {
          checkLine(false, `Registry not reachable: ${e.message}`);
        }

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

  // -----------------------------------------------------------------------
  // mesh demote — revert to cluster-local registry
  // -----------------------------------------------------------------------
  cmd
    .command("demote")
    .description("Demote the registry back to cluster-local (remove public endpoints)")
    .action(async () => {
      banner("AzureClaw · Mesh Demote", "Demote Registry to Local");

      const ctx = loadContext();
      if (!ctx?.aksCluster || !ctx?.resourceGroup) {
        console.error(chalk.red("  ✘ No deployment context found."));
        process.exit(1);
      }

      if (ctx.registryMode !== "global") {
        console.log(chalk.yellow("  ⚠ Registry is already local."));
        return;
      }

      if (ctx.promoteMode === "port-forward") {
        // Kill port-forward processes
        section("Stopping Port-Forward Tunnels");
        const pidFile = path.join(os.homedir(), ".azureclaw", "port-forward-pids.json");
        if (fs.existsSync(pidFile)) {
          try {
            const pids = JSON.parse(fs.readFileSync(pidFile, "utf-8")) as Record<string, number>;
            for (const [label, pid] of Object.entries(pids)) {
              try {
                process.kill(pid, "SIGTERM");
                checkLine(true, `${label} tunnel stopped (PID ${pid})`);
              } catch {
                console.log(chalk.dim(`  · ${label} tunnel already stopped (PID ${pid})`));
              }
            }
            fs.unlinkSync(pidFile);
          } catch {
            console.log(chalk.yellow("  ⚠ Could not read PID file"));
          }
        } else {
          console.log(chalk.dim("  · No PID file found (tunnels may have exited)"));
        }
      } else {
        // LoadBalancer mode: revert services to ClusterIP
        section("Reverting Services to ClusterIP");
        const services = ["agentmesh-registry", "agentmesh-relay"];
        for (const svc of services) {
          try {
            await execa("kubectl", [
              "patch", "svc", svc, "-n", "agentmesh",
              "--type", "merge",
              "-p", JSON.stringify({ spec: { type: "ClusterIP", loadBalancerSourceRanges: null } }),
            ], { stdio: "pipe" });
            checkLine(true, `${svc} → ClusterIP`);
          } catch {
            console.log(chalk.yellow(`  ⚠ Could not revert ${svc}`));
          }
        }

        // Clean up any leftover Ingress resources from earlier attempts
        const ingressResources = [
          "ingress/agentmesh-registry-ingress",
          "ingress/agentmesh-relay-ingress",
        ];
        for (const resource of ingressResources) {
          try {
            await execa("kubectl", [
              "delete", resource, "-n", "agentmesh", "--ignore-not-found",
            ], { stdio: "pipe" });
          } catch { /* ignore */ }
        }
      }

      // Update deployment context
      ctx.registryMode = "local";
      ctx.globalRegistryUrl = undefined;
      ctx.globalRelayUrl = undefined;
      delete ctx.promoteMode;
      saveContext(ctx);

      section("Status");
      kvLine("Registry mode", "local (cluster-only)");

      console.log();
      console.log(chalk.green("  ✓ ") + chalk.bold("Registry demoted to local."));
      console.log(chalk.dim("    Public endpoints removed. Agents in this cluster still work."));
      console.log(chalk.dim("    Cross-environment handoff is no longer available."));
      console.log();
    });

  return cmd;
}

// Exported for testing
export { generateKeypair, base58Encode, encryptPrivateKey, decryptPrivateKey };
