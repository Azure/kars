/**
 * Mesh identity management — Ed25519/X25519 keypair generation and persistence.
 *
 * Identity is stored encrypted (AES-256-GCM) at ~/.azureclaw/identity.json.
 * The encryption key is derived from machine-specific seed (hostname + homedir).
 */

import * as crypto from "node:crypto";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

const IDENTITY_DIR = path.join(os.homedir(), ".azureclaw");
const IDENTITY_FILE = path.join(IDENTITY_DIR, "identity.json");

export interface MeshIdentityData {
  amid: string;
  signingPublicKey: string;
  /** AES-256-GCM encrypted signing private key */
  encryptedSigningKey: string;
  iv: string;
  authTag: string;
  createdAt: string;
}

export interface MeshIdentity {
  amid: string;
  signingPublicKey: Buffer;
  signingPrivateKey: Buffer;
}

// ---------------------------------------------------------------------------
// AMID derivation — matches the SDK: base58(sha256(pubkey)[:20])
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
  // Leading zeros
  for (const b of bytes) {
    if (b === 0) result = BASE58_ALPHABET[0] + result;
    else break;
  }
  return result;
}

export function deriveAmid(signingPublicKey: Buffer): string {
  const hash = crypto.createHash("sha256").update(signingPublicKey).digest();
  return encodeBase58(hash.subarray(0, 20));
}

// ---------------------------------------------------------------------------
// Encryption helpers
// ---------------------------------------------------------------------------

function deriveEncryptionKey(): Buffer {
  const seed = `azureclaw:mesh-identity:${os.hostname()}:${os.homedir()}`;
  return crypto.createHash("sha256").update(seed).digest();
}

function encrypt(data: Buffer): { encrypted: string; iv: string; authTag: string } {
  const key = deriveEncryptionKey();
  const iv = crypto.randomBytes(12);
  const cipher = crypto.createCipheriv("aes-256-gcm", key, iv);
  const encrypted = Buffer.concat([cipher.update(data), cipher.final()]);
  return {
    encrypted: encrypted.toString("base64"),
    iv: iv.toString("base64"),
    authTag: cipher.getAuthTag().toString("base64"),
  };
}

function decrypt(encrypted: string, iv: string, authTag: string): Buffer {
  const key = deriveEncryptionKey();
  const decipher = crypto.createDecipheriv(
    "aes-256-gcm",
    key,
    Buffer.from(iv, "base64")
  );
  decipher.setAuthTag(Buffer.from(authTag, "base64"));
  const decrypted = Buffer.concat([
    decipher.update(Buffer.from(encrypted, "base64")),
    decipher.final(),
  ]);
  return decrypted;
}

// ---------------------------------------------------------------------------
// Generate / Load / Save
// ---------------------------------------------------------------------------

/** Generate a new Ed25519 mesh identity. */
export function generateIdentity(): MeshIdentity {
  const { publicKey, privateKey } = crypto.generateKeyPairSync("ed25519");
  const pubRaw = publicKey.export({ type: "spki", format: "der" });
  const privRaw = privateKey.export({ type: "pkcs8", format: "der" });
  // Extract raw 32-byte keys from DER encoding
  const pubBytes = pubRaw.subarray(pubRaw.length - 32);
  const privBytes = privRaw.subarray(privRaw.length - 32);
  const amid = deriveAmid(pubBytes);
  return {
    amid,
    signingPublicKey: pubBytes,
    signingPrivateKey: privBytes,
  };
}

/** Save identity to ~/.azureclaw/identity.json (encrypted). */
export function saveIdentity(identity: MeshIdentity): void {
  fs.mkdirSync(IDENTITY_DIR, { recursive: true });
  const { encrypted, iv, authTag } = encrypt(identity.signingPrivateKey);
  const data: MeshIdentityData = {
    amid: identity.amid,
    signingPublicKey: identity.signingPublicKey.toString("base64"),
    encryptedSigningKey: encrypted,
    iv,
    authTag,
    createdAt: new Date().toISOString(),
  };
  fs.writeFileSync(IDENTITY_FILE, JSON.stringify(data, null, 2), { mode: 0o600 });
}

/** Load identity from ~/.azureclaw/identity.json. Returns null if not found. */
export function loadIdentity(): MeshIdentity | null {
  if (!fs.existsSync(IDENTITY_FILE)) return null;
  try {
    const raw = fs.readFileSync(IDENTITY_FILE, "utf-8");
    const data: MeshIdentityData = JSON.parse(raw);
    const signingPrivateKey = decrypt(data.encryptedSigningKey, data.iv, data.authTag);
    const signingPublicKey = Buffer.from(data.signingPublicKey, "base64");
    return {
      amid: data.amid,
      signingPublicKey,
      signingPrivateKey,
    };
  } catch {
    return null;
  }
}

/** Load existing identity or generate a new one. */
export function loadOrCreateIdentity(): MeshIdentity {
  const existing = loadIdentity();
  if (existing) return existing;
  const identity = generateIdentity();
  saveIdentity(identity);
  return identity;
}

/** Get the identity file path (for display). */
export function getIdentityPath(): string {
  return IDENTITY_FILE;
}
