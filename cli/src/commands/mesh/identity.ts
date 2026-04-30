// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Phase 2 / S15.b: identity persistence + at-rest crypto + AMID
// derivation extracted from mesh.ts. Public surface preserved
// (re-exported from mesh.ts) to keep mesh.test.ts imports stable.

import * as fs from "node:fs";
import * as path from "node:path";
import * as os from "node:os";
import * as crypto from "node:crypto";

export const IDENTITY_DIR = path.join(os.homedir(), ".azureclaw");
export const IDENTITY_FILE = path.join(IDENTITY_DIR, "mesh-identity.json");

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

export function encryptPrivateKey(privateKey: Buffer): {
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

export function decryptPrivateKey(identity: MeshIdentity): Buffer {
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

export function generateKeypair(): {
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

export function base58Encode(buffer: Buffer): string {
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

export function loadIdentity(): MeshIdentity | null {
  if (!fs.existsSync(IDENTITY_FILE)) return null;
  try {
    const data = JSON.parse(fs.readFileSync(IDENTITY_FILE, "utf-8"));
    return data as MeshIdentity;
  } catch {
    return null;
  }
}

export function saveIdentity(identity: MeshIdentity): void {
  fs.mkdirSync(IDENTITY_DIR, { recursive: true, mode: 0o700 });
  fs.writeFileSync(IDENTITY_FILE, JSON.stringify(identity, null, 2), {
    mode: 0o600,
  });
}
