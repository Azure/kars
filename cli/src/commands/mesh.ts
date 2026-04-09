import { Command } from "commander";
import chalk from "chalk";
import * as fs from "node:fs";
import * as path from "node:path";
import * as os from "node:os";
import * as http from "node:http";
import * as crypto from "node:crypto";
import { execa } from "execa";
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
    .option("--allow-ip <cidr>", "Restrict access to this IP/CIDR (e.g. 203.0.113.42 or 203.0.113.0/24). Omit for auto-detect.")
    .option("--domain <domain>", "Custom domain (e.g. mesh.example.com). Omit for auto sslip.io.")
    .action(async (opts: { allowIp?: string; domain?: string }) => {
      banner("AzureClaw · Mesh Promote", "Promote Registry to Global");

      // Load deployment context
      const ctx = loadContext();
      if (!ctx?.aksCluster || !ctx?.resourceGroup) {
        console.error(chalk.red("  ✘ No deployment context found."));
        console.error(chalk.dim("    Run azureclaw up first to deploy an AKS cluster."));
        process.exit(1);
      }

      if (ctx.registryMode === "global" && ctx.globalRegistryUrl) {
        console.log(chalk.yellow("  ⚠ Registry is already global."));
        kvLine("Registry", ctx.globalRegistryUrl);
        kvLine("Relay", ctx.globalRelayUrl ?? "—");
        return;
      }

      section("Cluster");
      kvLine("AKS", ctx.aksCluster);
      kvLine("Resource Group", ctx.resourceGroup);
      kvLine("ACR", ctx.acrLoginServer ?? "—");

      // Verify agentmesh namespace exists
      section("AgentMesh");
      try {
        await execa("kubectl", [
          "get", "namespace", "agentmesh",
        ], { stdio: "pipe" });
        checkLine(true, "agentmesh namespace exists");
      } catch {
        console.error(chalk.red("  ✘ agentmesh namespace not found."));
        console.error(chalk.dim("    Deploy an agent first: azureclaw up <name> --model <model>"));
        process.exit(1);
      }

      // Verify registry and relay pods are running
      try {
        await execa("kubectl", [
          "get", "pod", "-n", "agentmesh", "-l", "app=agentmesh-registry",
          "--field-selector", "status.phase=Running", "-o", "name",
        ], { stdio: "pipe" });
        checkLine(true, "Registry pod running");
      } catch {
        console.error(chalk.red("  ✘ Registry pod not running."));
        process.exit(1);
      }

      try {
        await execa("kubectl", [
          "get", "pod", "-n", "agentmesh", "-l", "app=agentmesh-relay",
          "--field-selector", "status.phase=Running", "-o", "name",
        ], { stdio: "pipe" });
        checkLine(true, "Relay pod running");
      } catch {
        console.error(chalk.red("  ✘ Relay pod not running."));
        process.exit(1);
      }

      // Resolve IP allowlist
      section("Access Control");
      let allowCidr: string | null = null;

      if (opts.allowIp) {
        allowCidr = opts.allowIp.includes("/") ? opts.allowIp : `${opts.allowIp}/32`;
        kvLine("Allow IP", allowCidr + " (from --allow-ip)");
      } else {
        // Auto-detect public IP
        try {
          const resp = await fetch("https://ifconfig.me/ip", { signal: AbortSignal.timeout(5000) });
          if (resp.ok) {
            const ip = (await resp.text()).trim();
            if (/^\d{1,3}(\.\d{1,3}){3}$/.test(ip)) {
              allowCidr = `${ip}/32`;
              kvLine("Allow IP", allowCidr + " (auto-detected)");
            }
          }
        } catch { /* fall through to unrestricted */ }

        if (!allowCidr) {
          console.log(chalk.yellow("  ⚠ Could not detect public IP — registry will be open."));
          console.log(chalk.dim("    Re-run with --allow-ip <your-ip> to restrict access."));
        }
      }

      // Find the ingress manifest
      section("Ingress");
      // Compiled JS is at cli/dist/commands/mesh.js — go up 3 levels to repo root
      const cliDir = path.dirname(new URL(import.meta.url).pathname);
      const repoRoot = path.resolve(cliDir, "..", "..", "..");
      const ingressManifest = path.join(repoRoot, "deploy", "agentmesh-ingress.yaml");

      if (!fs.existsSync(ingressManifest)) {
        console.error(chalk.red(`  ✘ Ingress manifest not found: ${ingressManifest}`));
        process.exit(1);
      }

      // Resolve domain — custom or auto-detect via AppGW public IP + sslip.io
      let domain: string;
      if (opts.domain) {
        domain = opts.domain;
        kvLine("Domain", domain + " (custom)");
      } else {
        // Find the Application Gateway public IP
        console.log(chalk.dim("  Detecting AppGW public IP..."));
        let appGwIp = "";
        try {
          // AGIC uses an AppGW whose public IP we can find via the AKS add-on
          const { stdout: appGwName } = await execa("az", [
            "aks", "show",
            "--resource-group", ctx.resourceGroup,
            "--name", ctx.aksCluster,
            "--query", "addonProfiles.ingressApplicationGateway.config.applicationGatewayName",
            "--output", "tsv",
          ], { stdio: "pipe", timeout: 15000 });

          if (appGwName.trim()) {
            // Get the frontend IP config → public IP resource ID
            const { stdout: pipId } = await execa("az", [
              "network", "application-gateway", "show",
              "--resource-group", ctx.resourceGroup,
              "--name", appGwName.trim(),
              "--query", "frontendIPConfigurations[0].publicIPAddress.id",
              "--output", "tsv",
            ], { stdio: "pipe", timeout: 15000 });

            if (pipId.trim()) {
              const { stdout: ip } = await execa("az", [
                "network", "public-ip", "show",
                "--ids", pipId.trim(),
                "--query", "ipAddress",
                "--output", "tsv",
              ], { stdio: "pipe", timeout: 10000 });
              appGwIp = ip.trim();
            }
          }
        } catch { /* fall through */ }

        // Fallback: check existing Ingress for an IP
        if (!appGwIp) {
          try {
            const { stdout: ingressIp } = await execa("kubectl", [
              "get", "ingress", "-n", "agentmesh",
              "-o", "jsonpath={.items[0].status.loadBalancer.ingress[0].ip}",
            ], { stdio: "pipe", timeout: 10000 });
            if (ingressIp.trim() && /^\d/.test(ingressIp.trim())) {
              appGwIp = ingressIp.trim();
            }
          } catch { /* fall through */ }
        }

        if (!appGwIp) {
          console.error(chalk.red("  ✘ Could not detect AppGW public IP."));
          console.error(chalk.dim("    Use --domain <your-domain> to specify manually."));
          process.exit(1);
        }

        // sslip.io: dots in IP replaced with dashes
        const sslipHost = appGwIp.replace(/\./g, "-") + ".sslip.io";
        domain = sslipHost;
        kvLine("AppGW IP", appGwIp);
        kvLine("Domain", domain + " (sslip.io — auto)");
      }

      // Get subscription ID (for WAF policy reference in Ingress)
      const { stdout: subId } = await execa("az", [
        "account", "show", "--query", "id", "--output", "tsv",
      ], { stdio: "pipe", timeout: 10000 }).catch(() => ({ stdout: "" }));

      if (!subId.trim()) {
        console.error(chalk.red("  ✘ Cannot determine Azure subscription ID."));
        console.error(chalk.dim("    Run: az login"));
        process.exit(1);
      }

      // Patch and apply the ingress manifest
      let patchedIngress = fs.readFileSync(ingressManifest, "utf-8")
        .replace(/DOMAIN_PLACEHOLDER/g, domain)
        .replace(/SUBSCRIPTION_ID/g, subId.trim())
        .replace(/RESOURCE_GROUP/g, ctx.resourceGroup)
        .replace(/azureclawacr\.azurecr\.io/g, ctx.acrLoginServer ?? "azureclawacr.azurecr.io");

      // For sslip.io: disable TLS (no valid cert) and SSL redirect
      if (domain.endsWith(".sslip.io")) {
        // Remove TLS blocks (spec.tls and secretName lines)
        patchedIngress = patchedIngress.replace(/\s*tls:\n\s*- hosts:\n\s*-[^\n]*\n\s*secretName:[^\n]*/g, "");
        // Disable SSL redirect
        patchedIngress = patchedIngress.replace(
          /appgw\.ingress\.kubernetes\.io\/ssl-redirect: "true"/g,
          'appgw.ingress.kubernetes.io/ssl-redirect: "false"'
        );
      }

      // Inject IP allowlist annotation into both Ingress resources
      if (allowCidr) {
        patchedIngress = patchedIngress.replace(
          /kubernetes\.io\/ingress\.class: azure\/application-gateway/g,
          `kubernetes.io/ingress.class: azure/application-gateway\n    appgw.ingress.kubernetes.io/whitelist-source-range: "${allowCidr}"`
        );
      }

      const tmpIngress = path.join(os.tmpdir(), `.azureclaw-ingress-${Date.now()}.yaml`);
      try {
        fs.writeFileSync(tmpIngress, patchedIngress);
        await execa("kubectl", ["apply", "-f", tmpIngress], { stdio: "pipe" });
        checkLine(true, "Ingress + NetworkPolicies applied");
        if (allowCidr) {
          checkLine(true, `IP allowlist: ${allowCidr}`);
        }
      } catch (e: any) {
        console.error(chalk.red(`  ✘ kubectl apply failed: ${e.message}`));
        process.exit(1);
      } finally {
        try { fs.unlinkSync(tmpIngress); } catch { /* noop */ }
      }

      const proto = domain.endsWith(".sslip.io") ? "http" : "https";
      const wsProto = domain.endsWith(".sslip.io") ? "ws" : "wss";
      const globalRegistryUrl = `${proto}://registry.${domain}`;
      const globalRelayUrl = `${wsProto}://relay.${domain}`;

      // Update deployment context
      ctx.registryMode = "global";
      ctx.globalRegistryUrl = globalRegistryUrl;
      ctx.globalRelayUrl = globalRelayUrl;
      saveContext(ctx);

      section("Global Endpoints");
      kvLine("Registry", chalk.cyan(globalRegistryUrl));
      kvLine("Relay", chalk.cyan(globalRelayUrl));
      kvLine("Domain", domain);

      console.log();
      console.log(chalk.green("  ✓ ") + chalk.bold("Registry promoted to global."));
      if (domain.endsWith(".sslip.io")) {
        console.log(chalk.dim("    Using sslip.io for DNS (auto-resolved, no setup needed)."));
        console.log(chalk.dim("    Note: HTTP only (no TLS) — secured by IP allowlist."));
      } else {
        console.log(chalk.dim(`    DNS: point registry.${domain} and relay.${domain} to AppGW public IP`));
      }
      console.log(chalk.dim(`    Then: azureclaw dev --global-registry ${globalRegistryUrl}`));
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

      section("Removing Ingress");

      // Delete the ingress resources (reverse of promote)
      const ingressResources = [
        "ingress/agentmesh-registry",
        "ingress/agentmesh-relay",
        "networkpolicy/postgres-restrict",
        "networkpolicy/registry-restrict",
        "networkpolicy/relay-restrict",
      ];

      for (const resource of ingressResources) {
        try {
          await execa("kubectl", [
            "delete", resource, "-n", "agentmesh", "--ignore-not-found",
          ], { stdio: "pipe" });
          checkLine(true, `Deleted ${resource}`);
        } catch {
          console.log(chalk.yellow(`  ⚠ Could not delete ${resource}`));
        }
      }

      // Update deployment context
      ctx.registryMode = "local";
      ctx.globalRegistryUrl = undefined;
      ctx.globalRelayUrl = undefined;
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
