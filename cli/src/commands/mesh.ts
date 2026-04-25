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

// HTML-escape user-controlled strings before embedding in HTML responses
// (CWE-79: reflected-xss). Minimal escaper for untrusted text content.
function escapeHtml(s: unknown): string {
  return String(s ?? "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

// Strip CR/LF from untrusted data before logging so attackers can't forge
// log lines (CWE-117: log-injection). Classic pattern recognized by CodeQL.
function sanitizeForLog(s: unknown): string {
  return String(s ?? "")
    .replace(/\r/g, "")
    .replace(/\n/g, " ")
    .replace(/\t/g, " ");
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

            // Return a nice HTML page — escape user-controlled fields to
            // prevent reflected XSS (result comes from the registry redirect).
            res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
            res.end(`
              <html><body style="font-family: system-ui; text-align: center; padding-top: 80px;">
                <h2>${result.success ? "✅ Authenticated!" : "❌ Authentication failed"}</h2>
                <p>${result.success ? "You can close this tab and return to the terminal." : escapeHtml(result.error ?? "Unknown error")}</p>
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
// Port & health helpers
// ---------------------------------------------------------------------------

/** Kill all processes listening on the given ports (prevents duplicate port-forwards). */
async function killProcessesOnPorts(ports: number[]): Promise<void> {
  for (const port of ports) {
    try {
      // -sTCP:LISTEN — only kill LISTENERS, not processes with active connections through the port
      const { stdout } = await execa("lsof", ["-ti", `:${port}`, "-sTCP:LISTEN"], { stdio: "pipe", reject: false });
      const pidsOnPort = stdout.trim().split("\n").filter(Boolean);
      for (const p of pidsOnPort) {
        try { process.kill(parseInt(p, 10), "SIGTERM"); } catch { /* already dead */ }
      }
      if (pidsOnPort.length > 0) {
        console.log(chalk.dim(`  · Cleared ${pidsOnPort.length} listener(s) on port ${port}`));
      }
    } catch { /* no process on port */ }
  }
  if (ports.length > 0) await new Promise(r => setTimeout(r, 500));
}

/** Post-start cleanup: kill any listener on our ports that isn't one of our spawned PIDs. */
async function killStaleListeners(portPidMap: Array<{ port: number; pid: number }>): Promise<void> {
  const ownPids = new Set(portPidMap.map(pp => pp.pid));
  const ports = [...new Set(portPidMap.map(pp => pp.port))];
  for (const port of ports) {
    try {
      const { stdout } = await execa("lsof", ["-ti", `:${port}`, "-sTCP:LISTEN"], { stdio: "pipe", reject: false });
      const listeners = stdout.trim().split("\n").filter(Boolean).map(Number);
      for (const pid of listeners) {
        if (!ownPids.has(pid)) {
          try { process.kill(pid, "SIGKILL"); } catch { /* already dead */ }
          console.log(chalk.dim(`  · Killed stale listener on port ${port} (PID ${pid})`));
        }
      }
    } catch { /* ignore */ }
  }
}

/** Find ports with multiple listeners (indicates stale port-forwards). */
async function findDuplicateListeners(ports: number[]): Promise<Array<{ port: number; count: number; pids: number[] }>> {
  const results: Array<{ port: number; count: number; pids: number[] }> = [];
  for (const port of ports) {
    try {
      const { stdout } = await execa("lsof", ["-ti", `:${port}`, "-sTCP:LISTEN"], { stdio: "pipe", reject: false });
      const pids = stdout.trim().split("\n").filter(Boolean).map(Number);
      if (pids.length > 1) {
        results.push({ port, count: pids.length, pids });
      }
    } catch { /* ignore */ }
  }
  return results;
}

/** Check registry health via HTTP /v1/health endpoint. */
async function checkRegistryHealth(port: number): Promise<boolean> {
  try {
    const resp = await fetch(`http://localhost:${port}/v1/health`, {
      signal: AbortSignal.timeout(5000),
    });
    if (resp.ok) {
      const body = await resp.json() as Record<string, unknown>;
      checkLine(true, `Registry healthy (${body.agents_registered ?? 0} agents, ${body.agents_online ?? 0} online)`);
      return true;
    }
    checkLine(false, `Registry returned HTTP ${resp.status}`);
    return false;
  } catch (e: unknown) {
    checkLine(false, `Registry not reachable: ${e instanceof Error ? e.message : String(e)}`);
    return false;
  }
}

/** Check relay health via WebSocket upgrade (not just TCP connect). */
async function checkRelayHealth(port: number): Promise<boolean> {
  return new Promise((resolve) => {
    const req = http.request({
      hostname: "127.0.0.1",
      port,
      path: "/",
      method: "GET",
      headers: {
        "Upgrade": "websocket",
        "Connection": "Upgrade",
        "Sec-WebSocket-Key": crypto.randomBytes(16).toString("base64"),
        "Sec-WebSocket-Version": "13",
      },
      timeout: 5000,
    });

    req.on("upgrade", (_res, socket) => {
      checkLine(true, `Relay healthy (WebSocket upgrade on localhost:${port})`);
      socket.destroy();
      resolve(true);
    });

    req.on("response", (res) => {
      // Got an HTTP response instead of upgrade — relay is serving but not WS
      if (res.statusCode === 101) {
        checkLine(true, `Relay healthy (localhost:${port})`);
        resolve(true);
      } else {
        checkLine(false, `Relay returned HTTP ${res.statusCode} (expected WebSocket upgrade)`);
        resolve(false);
      }
    });

    req.on("error", (e) => {
      checkLine(false, `Relay not reachable: ${e.message}`);
      resolve(false);
    });

    req.on("timeout", () => {
      checkLine(false, `Relay timeout on localhost:${port}`);
      req.destroy();
      resolve(false);
    });

    req.end();
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
            chalk.red(`  ✘ Verification failed: ${sanitizeForLog(result.error ?? "Unknown error")}`)
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
  // mesh list — show cluster pairings and offload sandboxes
  // -----------------------------------------------------------------------
  cmd
    .command("list")
    .description("List mesh pairings and offload sandboxes on the cluster")
    .action(async () => {
      banner("AzureClaw · Mesh List", "Pairings & Offload Sandboxes");

      const ns = "azureclaw-system";

      // Pairings
      try {
        const { stdout } = await execa("kubectl", [
          "get", "clawpairings", "-n", ns, "-o", "json",
        ], { stdio: "pipe" });
        const list = JSON.parse(stdout);
        const items = list.items as Array<{
          metadata: { name: string; creationTimestamp: string };
          spec: { tokenBudget?: number };
          status?: { phase?: string; boundAmid?: string; slotsUsed?: number; lastOffloadAt?: string };
        }>;

        section("Pairings");
        if (items.length === 0) {
          console.log(chalk.dim("  No pairings found."));
        } else {
          for (const p of items) {
            const phase = p.status?.phase || "Unknown";
            const amid = p.status?.boundAmid || "-";
            const budget = p.spec.tokenBudget ?? 0;
            const slots = p.status?.slotsUsed ?? 0;
            const icon = phase === "Active" ? "🟢" : phase === "PendingPairing" ? "🟡" : "⚪";
            console.log(`  ${icon} ${chalk.bold(p.metadata.name)}`);
            kvLine("  Phase", phase);
            kvLine("  AMID", amid);
            kvLine("  Budget", budget.toLocaleString() + " tokens");
            kvLine("  Offloads", String(slots));
            if (p.status?.lastOffloadAt) kvLine("  Last offload", p.status.lastOffloadAt);
            kvLine("  Age", p.metadata.creationTimestamp);
            console.log();
          }
        }
      } catch {
        console.log(chalk.red("  Failed to list pairings (is kubectl connected to the cluster?)"));
      }

      // Offload sandboxes
      try {
        const { stdout } = await execa("kubectl", [
          "get", "clawsandboxes", "-n", ns, "-o", "json",
        ], { stdio: "pipe" });
        const list = JSON.parse(stdout);
        const items = list.items as Array<{
          metadata: { name: string; creationTimestamp: string; labels?: Record<string, string>; annotations?: Record<string, string> };
          status?: { phase?: string };
        }>;
        const offloads = items.filter((s) => s.metadata.labels?.["azureclaw.azure.com/spawned-by"] === "offload");

        section("Offload Sandboxes");
        if (offloads.length === 0) {
          console.log(chalk.dim("  No offload sandboxes."));
        } else {
          for (const s of offloads) {
            const phase = s.status?.phase || "Unknown";
            const requester = s.metadata.labels?.["azureclaw.azure.com/offload-requester"] || "-";
            const task = s.metadata.annotations?.["azureclaw.azure.com/offload-task"] || "-";
            const icon = phase === "Running" ? "🟢" : phase === "Pending" ? "🟡" : "🔴";
            console.log(`  ${icon} ${chalk.bold(s.metadata.name)}`);
            kvLine("  Phase", phase);
            kvLine("  Requester", requester);
            kvLine("  Task", task.length > 80 ? task.substring(0, 77) + "..." : task);
            kvLine("  Created", s.metadata.creationTimestamp);
            console.log();
          }
        }
      } catch {
        console.log(chalk.red("  Failed to list sandboxes"));
      }

      // Leader info
      try {
        const { stdout } = await execa("kubectl", [
          "get", "lease", "azureclaw-mesh-peer-leader", "-n", ns,
          "-o", "jsonpath={.spec.holderIdentity}",
        ], { stdio: "pipe" });
        if (stdout) {
          section("Mesh Peer");
          kvLine("Leader", stdout);
        }
      } catch { /* lease may not exist */ }
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
  // mesh security — toggle REQUIRE_REGISTRATION on the relay deployment
  //
  // Context: when REQUIRE_REGISTRATION=true, only agents that have been
  // registered with the mesh registry can connect to the relay. This is
  // the secure default. In transient states (registry DB wipe, fresh
  // cluster bootstrap) the controller and NemoClaw instances may need a
  // window to re-register. Use `mesh security open` temporarily to unblock.
  // -----------------------------------------------------------------------
  cmd
    .command("security <mode>")
    .description(
      "Toggle relay REQUIRE_REGISTRATION (mode: open | strict | status)"
    )
    .option("-n, --namespace <ns>", "AgentMesh namespace", "agentmesh")
    .option("--deployment <name>", "Relay deployment name", "relay")
    .action(async (mode: string, opts: { namespace: string; deployment: string }) => {
      banner("AzureClaw · Mesh Security", "Relay registration enforcement");

      const normalized = mode.toLowerCase();
      if (!["open", "strict", "status"].includes(normalized)) {
        console.error(chalk.red(`  ✘ Unknown mode "${mode}". Use: open, strict, or status.`));
        process.exit(1);
      }

      // Read current value
      let current = "(unknown)";
      try {
        const { stdout } = await execa(
          "kubectl",
          [
            "get",
            "deployment",
            opts.deployment,
            "-n",
            opts.namespace,
            "-o",
            "jsonpath={.spec.template.spec.containers[?(@.name==\"relay\")].env[?(@.name==\"REQUIRE_REGISTRATION\")].value}",
          ],
          { stdio: "pipe" }
        );
        current = stdout.trim() || "(unset, defaults to true)";
      } catch (e: unknown) {
        console.error(
          chalk.red(
            `  ✘ Could not read relay deployment ${opts.namespace}/${opts.deployment}: ${
              e instanceof Error ? e.message : String(e)
            }`
          )
        );
        process.exit(1);
      }

      kvLine("Namespace", opts.namespace);
      kvLine("Deployment", opts.deployment);
      kvLine("Current REQUIRE_REGISTRATION", current);

      if (normalized === "status") {
        const isStrict = current === "true" || current.startsWith("(unset");
        console.log();
        console.log(
          isStrict
            ? chalk.green("  🔒 strict — only registered agents may connect")
            : chalk.yellow("  🔓 open — any signed agent may connect (insecure; use for bootstrap only)")
        );
        return;
      }

      const target = normalized === "strict" ? "true" : "false";
      if (current === target) {
        console.log(chalk.dim(`  Already ${normalized}. No change.`));
        return;
      }

      if (normalized === "open") {
        const { default: inquirer } = await import("inquirer");
        const { confirm } = await inquirer.prompt([
          {
            type: "confirm",
            name: "confirm",
            message:
              "Switching to OPEN mode lets any signed agent connect. This should only be used temporarily during bootstrap or recovery. Continue?",
            default: false,
          },
        ]);
        if (!confirm) {
          console.log(chalk.dim("  Cancelled."));
          return;
        }
      }

      section("Applying change");
      try {
        await execa(
          "kubectl",
          [
            "set",
            "env",
            `deployment/${opts.deployment}`,
            "-n",
            opts.namespace,
            `REQUIRE_REGISTRATION=${target}`,
          ],
          { stdio: "inherit" }
        );
        checkLine(true, `REQUIRE_REGISTRATION=${target}`);

        // Wait for rollout to complete so the setting is active before we return
        await execa(
          "kubectl",
          [
            "rollout",
            "status",
            `deployment/${opts.deployment}`,
            "-n",
            opts.namespace,
            "--timeout=120s",
          ],
          { stdio: "inherit" }
        );

        console.log();
        console.log(
          normalized === "open"
            ? chalk.yellow(
                "  ⚠️  Mode: OPEN — revert with `azureclaw mesh security strict` once bootstrap is complete."
              )
            : chalk.green(
                "  🔒 Mode: STRICT — only registered agents may connect."
              )
        );
        console.log(
          chalk.dim(
            "  Note: `azureclaw up` re-applies deploy/agentmesh.yaml which resets this to the manifest default."
          )
        );
      } catch (e: unknown) {
        console.error(
          chalk.red(
            `  ✘ Failed to apply change: ${e instanceof Error ? e.message : String(e)}`
          )
        );
        process.exit(1);
      }
    });

  // -----------------------------------------------------------------------
  // mesh peer — toggle controller federation (MESH_PEER_ENABLED)
  // -----------------------------------------------------------------------
  cmd
    .command("peer <mode>")
    .description(
      "Toggle controller mesh federation (mode: enable | disable | status). When disabled, external agents cannot pair."
    )
    .option("-n, --namespace <ns>", "Controller namespace", "azureclaw-system")
    .option("--deployment <name>", "Controller deployment name", "azureclaw-controller")
    .action(async (mode: string, opts: { namespace: string; deployment: string }) => {
      banner("AzureClaw · Mesh Peer", "Controller federation (pair_request handler)");

      const normalized = mode.toLowerCase();
      if (!["enable", "disable", "status", "on", "off"].includes(normalized)) {
        console.error(chalk.red(`  ✘ Unknown mode "${mode}". Use: enable, disable, or status.`));
        process.exit(1);
      }
      const want = normalized === "status"
        ? "status"
        : (normalized === "enable" || normalized === "on" ? "enable" : "disable");

      // Read current value
      let current = "(unknown)";
      try {
        const { stdout } = await execa(
          "kubectl",
          [
            "get",
            "deployment",
            opts.deployment,
            "-n",
            opts.namespace,
            "-o",
            "jsonpath={.spec.template.spec.containers[0].env[?(@.name==\"MESH_PEER_ENABLED\")].value}",
          ],
          { stdio: "pipe" }
        );
        current = stdout.trim() || "(unset, defaults to true)";
      } catch (e: unknown) {
        console.error(
          chalk.red(
            `  ✘ Could not read controller deployment ${opts.namespace}/${opts.deployment}: ${
              e instanceof Error ? e.message : String(e)
            }`
          )
        );
        process.exit(1);
      }

      kvLine("Namespace", opts.namespace);
      kvLine("Deployment", opts.deployment);
      kvLine("Current MESH_PEER_ENABLED", current);

      if (want === "status") {
        const isEnabled = current === "true" || current.startsWith("(unset");
        console.log();
        console.log(
          isEnabled
            ? chalk.green("  🔗 enabled — controller joins the relay and answers pair_request messages")
            : chalk.yellow("  🚫 disabled — external agent pairing will NOT work")
        );
        return;
      }

      const target = want === "enable" ? "true" : "false";
      if (current === target) {
        console.log(chalk.dim(`  Already ${want}d. No change.`));
        return;
      }

      section("Applying change");
      try {
        await execa(
          "kubectl",
          [
            "set",
            "env",
            `deployment/${opts.deployment}`,
            "-n",
            opts.namespace,
            `MESH_PEER_ENABLED=${target}`,
          ],
          { stdio: "inherit" }
        );
        checkLine(true, `MESH_PEER_ENABLED=${target}`);

        // Wait for rollout so the setting is active before we return
        await execa(
          "kubectl",
          [
            "rollout",
            "status",
            `deployment/${opts.deployment}`,
            "-n",
            opts.namespace,
            "--timeout=120s",
          ],
          { stdio: "inherit" }
        );

        console.log();
        console.log(
          want === "enable"
            ? chalk.green(
                "  🔗 Mesh peer ENABLED — controller will join the relay shortly and pair_request messages will be answered."
              )
            : chalk.yellow(
                "  🚫 Mesh peer DISABLED — external agent pairing will not work. Re-enable with `azureclaw mesh peer enable`."
              )
        );
        console.log(
          chalk.dim(
            "  Note: `azureclaw up` re-applies Helm values (default: enabled). Pass --no-mesh-peer to keep it disabled."
          )
        );
      } catch (e: unknown) {
        console.error(
          chalk.red(
            `  ✘ Failed to apply change: ${e instanceof Error ? e.message : String(e)}`
          )
        );
        process.exit(1);
      }
    });

  // -----------------------------------------------------------------------
  // mesh unpair — delete cluster pairings
  // -----------------------------------------------------------------------
  cmd
    .command("unpair")
    .description("Delete mesh pairings from the AKS cluster")
    .option("--all", "Delete all pairings without prompting")
    .option("--name <name>", "Delete a specific pairing by name")
    .action(async (opts: { all?: boolean; name?: string }) => {
      banner("AzureClaw · Mesh Unpair", "Remove Pairings");

      const ns = "azureclaw-system";
      try {
        const { stdout } = await execa("kubectl", [
          "get", "clawpairings", "-n", ns,
          "-o", "json",
        ], { stdio: "pipe" });
        const list = JSON.parse(stdout);
        const items = list.items as Array<{ metadata: { name: string }; status?: { phase?: string; boundAmid?: string } }>;

        if (items.length === 0) {
          console.log(chalk.dim("  No pairings found."));
          return;
        }

        if (opts.name) {
          const match = items.find((p) => p.metadata.name === opts.name);
          if (!match) {
            console.log(chalk.red(`  Pairing "${opts.name}" not found.`));
            console.log(chalk.dim(`  Available: ${items.map((p) => p.metadata.name).join(", ")}`));
            return;
          }
          await execa("kubectl", ["delete", "clawpairing", opts.name, "-n", ns], { stdio: "pipe" });
          checkLine(true, `Deleted pairing: ${opts.name}`);
          return;
        }

        // Show pairings
        console.log();
        for (const p of items) {
          const phase = p.status?.phase || "Unknown";
          const amid = p.status?.boundAmid || "-";
          const icon = phase === "Active" ? "🟢" : phase === "PendingPairing" ? "🟡" : "⚪";
          console.log(`  ${icon} ${chalk.bold(p.metadata.name)}  ${chalk.dim(phase)}  ${chalk.dim(amid)}`);
        }
        console.log();

        if (opts.all) {
          await execa("kubectl", ["delete", "clawpairings", "--all", "-n", ns], { stdio: "pipe" });
          checkLine(true, `Deleted all ${items.length} pairing(s)`);
          return;
        }

        const { default: inquirer } = await import("inquirer");
        const { targets } = await inquirer.prompt([
          {
            type: "checkbox",
            name: "targets",
            message: "Select pairings to delete:",
            choices: items.map((p) => ({
              name: `${p.metadata.name} (${p.status?.phase || "Unknown"})`,
              value: p.metadata.name,
            })),
          },
        ]);

        if (targets.length === 0) {
          console.log(chalk.dim("  No pairings selected."));
          return;
        }

        for (const name of targets) {
          await execa("kubectl", ["delete", "clawpairing", name, "-n", ns], { stdio: "pipe" });
          checkLine(true, `Deleted: ${name}`);
        }
      } catch (e: unknown) {
        const msg = e instanceof Error ? e.message : String(e);
        console.log(chalk.red(`  Failed to manage pairings: ${msg}`));
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
export { generateKeypair, base58Encode, encryptPrivateKey, decryptPrivateKey, checkRegistryHealth, checkRelayHealth, killProcessesOnPorts, killStaleListeners };
