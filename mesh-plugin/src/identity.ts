/**
 * Mesh identity management — Ed25519 signing keypairs.
 *
 * Uses Node.js built-in crypto for Ed25519 key generation — no external
 * dependency on @agentmesh/sdk. Identity is persisted encrypted at rest
 * with AES-256-GCM (key derived from hostname + homedir).
 *
 * Backward-compatible: reads legacy schema-2 files (from the old SDK-based
 * implementation) and migrates them to schema-3 on next save. The AMID and
 * signing keys are preserved across the migration.
 *
 * Identity implements IMeshIdentity so it can be passed directly to
 * AgtTransport without conversion.
 */

import * as crypto from "node:crypto";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import type { IMeshIdentity } from "./transport-interface.js";

const IDENTITY_DIR = path.join(os.homedir(), ".azureclaw");
const IDENTITY_FILE = path.join(IDENTITY_DIR, "identity.json");

// ---------------------------------------------------------------------------
// MeshIdentity — public shape consumed by connection.ts, index.ts, tests
// ---------------------------------------------------------------------------

export interface MeshIdentity extends IMeshIdentity {
  /** AgentMesh ID — base58(sha256(pubkey)[:20]). Primary routing address. */
  amid: string;
  /** Ed25519 signing public key (raw 32 bytes). */
  signingPublicKey: Uint8Array;
  /** Ed25519 signing private key (raw 32 bytes). */
  signingPrivateKey: Uint8Array;
}

// ---------------------------------------------------------------------------
// AMID / DID derivation
// ---------------------------------------------------------------------------

const BASE58_ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

function encodeBase58(bytes: Uint8Array): string {
  let num = BigInt(0);
  for (const b of bytes) {
    num = num * BigInt(256) + BigInt(b);
  }
  if (num === BigInt(0)) return BASE58_ALPHABET[0];
  let result = "";
  while (num > BigInt(0)) {
    const rem = Number(num % BigInt(58));
    result = BASE58_ALPHABET[rem] + result;
    num = num / BigInt(58);
  }
  for (const b of bytes) {
    if (b === 0) result = BASE58_ALPHABET[0] + result;
    else break;
  }
  return result;
}

export function deriveAmid(signingPublicKey: Uint8Array): string {
  const hash = crypto.createHash("sha256").update(signingPublicKey).digest();
  return encodeBase58(hash.subarray(0, 20));
}

// ---------------------------------------------------------------------------
// Encryption at rest
// ---------------------------------------------------------------------------

function deriveEncryptionKey(): Buffer {
  const seed = `azureclaw:mesh-identity:${os.hostname()}:${os.homedir()}`;
  return crypto.createHash("sha256").update(seed).digest();
}

// ---------------------------------------------------------------------------
// On-disk envelope formats
// ---------------------------------------------------------------------------

/** Legacy schema-2: full SDK IdentityData blob encrypted. */
interface LegacyEnvelope {
  schema: 2;
  ciphertext: string;
  iv: string;
  authTag: string;
  createdAt: string;
}

/** Schema-3: Ed25519 keys only, no SDK dependency. */
interface Envelope {
  schema: 3;
  signingPublicKey: string;  // hex
  signingPrivateKey: string; // hex, AES-256-GCM encrypted
  iv: string;
  authTag: string;
  amid: string;
  createdAt: string;
}

// ---------------------------------------------------------------------------
// Raw Ed25519 key extraction from Node.js DER structures
// ---------------------------------------------------------------------------

function rawEd25519PublicKey(keyObject: crypto.KeyObject): Uint8Array {
  const der = keyObject.export({ type: "spki", format: "der" });
  return new Uint8Array(der.subarray(der.length - 32));
}

function rawEd25519PrivateKey(keyObject: crypto.KeyObject): Uint8Array {
  const der = keyObject.export({ type: "pkcs8", format: "der" });
  return new Uint8Array(der.subarray(der.length - 32));
}

// ---------------------------------------------------------------------------
// Generate / Load / Save
// ---------------------------------------------------------------------------

/** Generate a fresh Ed25519 identity using Node.js crypto. */
export function generateIdentity(): MeshIdentity {
  const keyPair = crypto.generateKeyPairSync("ed25519");
  const signingPublicKey = rawEd25519PublicKey(keyPair.publicKey);
  const signingPrivateKey = rawEd25519PrivateKey(keyPair.privateKey);
  const amid = deriveAmid(signingPublicKey);
  return {
    agentId: amid,
    amid,
    signingPublicKey,
    signingPrivateKey,
  };
}

/** Persist identity to ~/.azureclaw/identity.json (schema-3 envelope). */
export function saveIdentity(identity: MeshIdentity): void {
  fs.mkdirSync(IDENTITY_DIR, { recursive: true });

  const key = deriveEncryptionKey();
  const iv = crypto.randomBytes(12);
  const cipher = crypto.createCipheriv("aes-256-gcm", key, iv);
  const encrypted = Buffer.concat([
    cipher.update(Buffer.from(identity.signingPrivateKey)),
    cipher.final(),
  ]);

  const envelope: Envelope = {
    schema: 3,
    signingPublicKey: Buffer.from(identity.signingPublicKey).toString("hex"),
    signingPrivateKey: encrypted.toString("hex"),
    iv: iv.toString("hex"),
    authTag: cipher.getAuthTag().toString("hex"),
    amid: identity.amid,
    createdAt: new Date().toISOString(),
  };

  fs.writeFileSync(IDENTITY_FILE, JSON.stringify(envelope, null, 2), { mode: 0o600 });
}

/**
 * Load identity from disk. Supports both schema-2 (legacy SDK) and schema-3.
 * Returns `null` if the file is missing or corrupt.
 */
export function loadIdentity(): MeshIdentity | null {
  if (!fs.existsSync(IDENTITY_FILE)) return null;
  try {
    const raw = JSON.parse(fs.readFileSync(IDENTITY_FILE, "utf-8"));

    if (raw.schema === 3) {
      return loadSchema3(raw as Envelope);
    }

    if (raw.schema === 2) {
      return loadSchema2(raw as LegacyEnvelope);
    }

    return null;
  } catch {
    return null;
  }
}

/** Load schema-3 (native format). */
function loadSchema3(envelope: Envelope): MeshIdentity | null {
  if (!envelope.signingPublicKey || !envelope.signingPrivateKey) return null;

  const key = deriveEncryptionKey();
  const decipher = crypto.createDecipheriv(
    "aes-256-gcm", key, Buffer.from(envelope.iv, "hex"),
  );
  decipher.setAuthTag(Buffer.from(envelope.authTag, "hex"));
  const decrypted = Buffer.concat([
    decipher.update(Buffer.from(envelope.signingPrivateKey, "hex")),
    decipher.final(),
  ]);

  const signingPublicKey = new Uint8Array(Buffer.from(envelope.signingPublicKey, "hex"));
  const signingPrivateKey = new Uint8Array(decrypted);
  const amid = deriveAmid(signingPublicKey);

  return { agentId: amid, amid, signingPublicKey, signingPrivateKey };
}

/** Load legacy schema-2 (old @agentmesh/sdk IdentityData blob). */
function loadSchema2(envelope: LegacyEnvelope): MeshIdentity | null {
  if (!envelope.ciphertext || !envelope.iv || !envelope.authTag) return null;

  const key = deriveEncryptionKey();
  const decipher = crypto.createDecipheriv(
    "aes-256-gcm", key, Buffer.from(envelope.iv, "base64"),
  );
  decipher.setAuthTag(Buffer.from(envelope.authTag, "base64"));
  const decrypted = Buffer.concat([
    decipher.update(Buffer.from(envelope.ciphertext, "base64")),
    decipher.final(),
  ]);

  const data = JSON.parse(decrypted.toString("utf8"));

  // SDK stored keys as "ed25519:<base64>" — strip prefix, decode
  const pubB64 = stripPrefix(data.signing_public_key ?? "", "ed25519:");
  const privB64 = stripPrefix(data.signing_private_key ?? "", "ed25519:");
  if (!pubB64 || !privB64) return null;

  const signingPublicKey = new Uint8Array(Buffer.from(pubB64, "base64"));
  const signingPrivateKey = new Uint8Array(Buffer.from(privB64, "base64"));
  if (signingPublicKey.length !== 32 || signingPrivateKey.length !== 32) return null;

  const amid = deriveAmid(signingPublicKey);
  return { agentId: amid, amid, signingPublicKey, signingPrivateKey };
}

function stripPrefix(value: string, prefix: string): string {
  return value.startsWith(prefix) ? value.slice(prefix.length) : value;
}

/**
 * Load existing identity or generate a fresh one. If a legacy schema-2
 * file is loaded, it is automatically migrated to schema-3 on save.
 */
export function loadOrCreateIdentity(): MeshIdentity {
  const existing = loadIdentity();
  if (existing) {
    // Auto-migrate legacy schema-2 → schema-3
    try {
      const raw = JSON.parse(fs.readFileSync(IDENTITY_FILE, "utf-8"));
      if (raw.schema === 2) {
        saveIdentity(existing);
        console.log("[mesh] Migrated identity from schema-2 to schema-3");
      }
    } catch { /* best effort */ }
    return existing;
  }
  const identity = generateIdentity();
  saveIdentity(identity);
  return identity;
}

/** Absolute path of the on-disk identity file (for display / diagnostics). */
export function getIdentityPath(): string {
  return IDENTITY_FILE;
}
