// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Mesh identity management — Ed25519 signing + X25519 exchange keypairs.
 *
 * Generates keys with Node.js native crypto (no SDK dep). Persists the
 * key material at ~/.azureclaw/identity.json encrypted at rest with
 * AES-256-GCM (key derived from hostname+homedir).
 *
 * Exposes a stable `MeshIdentity` facade (amid + raw signing keys) for
 * the rest of the plugin. AMID derivation matches the AgentMesh
 * convention: base58(sha256(signing_public_key)[:20]).
 */

import * as crypto from "node:crypto";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

import { deriveCanonicalDid } from "./did.js";

const IDENTITY_DIR = path.join(os.homedir(), ".azureclaw");
const IDENTITY_FILE = path.join(IDENTITY_DIR, "identity.json");

/**
 * On-disk identity data. Each key field is `"<algo>:<base64>"` —
 * format kept stable for back-compat with sandboxes still reading
 * the legacy schema-2 envelope.
 */
interface IdentityData {
  signing_public_key: string;
  signing_private_key: string;
  exchange_public_key: string;
  exchange_private_key: string;
}

/** Envelope written to disk — IdentityData encrypted with AES-256-GCM. */
interface MeshIdentityEnvelope {
  /** Envelope schema version. Bump when the on-disk format changes. */
  schema: 2;
  /** Opaque — base64 AES-256-GCM ciphertext of `JSON.stringify(IdentityData)` */
  ciphertext: string;
  iv: string;
  authTag: string;
  createdAt: string;
}

export interface MeshIdentity {
  /** AgentMesh ID — derived from the Ed25519 signing public key. */
  amid: string;
  /** AGT-compatible DID — `did:agentmesh:<fingerprint>`, derived from the same key. */
  did: string;
  /** Ed25519 signing public key (raw 32 bytes). */
  signingPublicKey: Buffer;
  /** Ed25519 signing private key (raw 32 bytes). */
  signingPrivateKey: Buffer;
}

// ---------------------------------------------------------------------------
// AMID derivation — matches the AgentMesh convention:
//   base58(sha256(signing_public_key)[:20]).
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

export function deriveAmid(signingPublicKey: Buffer): string {
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

function encryptJson(json: string): { ciphertext: string; iv: string; authTag: string } {
  const key = deriveEncryptionKey();
  const iv = crypto.randomBytes(12);
  const cipher = crypto.createCipheriv("aes-256-gcm", key, iv);
  const encrypted = Buffer.concat([cipher.update(Buffer.from(json, "utf8")), cipher.final()]);
  return {
    ciphertext: encrypted.toString("base64"),
    iv: iv.toString("base64"),
    authTag: cipher.getAuthTag().toString("base64"),
  };
}

function decryptJson(envelope: MeshIdentityEnvelope): string {
  const key = deriveEncryptionKey();
  const decipher = crypto.createDecipheriv(
    "aes-256-gcm",
    key,
    Buffer.from(envelope.iv, "base64")
  );
  decipher.setAuthTag(Buffer.from(envelope.authTag, "base64"));
  const decrypted = Buffer.concat([
    decipher.update(Buffer.from(envelope.ciphertext, "base64")),
    decipher.final(),
  ]);
  return decrypted.toString("utf8");
}

// ---------------------------------------------------------------------------
// Key extraction — JWK `x`/`d` are base64url; convert to standard base64
// and strip the "<algo>:" prefix used in the on-disk format.
// ---------------------------------------------------------------------------

function b64urlToBuffer(s: string): Buffer {
  const b64 = s.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - (s.length % 4)) % 4);
  return Buffer.from(b64, "base64");
}

function stripPrefix(value: string, prefix: string): string {
  return value.startsWith(prefix) ? value.slice(prefix.length) : value;
}

function rawFromPrefixedB64(value: string, prefix: string): Buffer {
  return Buffer.from(stripPrefix(value, prefix), "base64");
}

function rawEd25519Keys(): { pub: Buffer; priv: Buffer } {
  const kp = crypto.generateKeyPairSync("ed25519");
  type Jwk = { x?: string; d?: string };
  const pub = kp.publicKey.export({ format: "jwk" }) as Jwk;
  const priv = kp.privateKey.export({ format: "jwk" }) as Jwk;
  if (!pub.x || !priv.d) {
    throw new Error("Ed25519 JWK export missing x/d field");
  }
  return { pub: b64urlToBuffer(pub.x), priv: b64urlToBuffer(priv.d) };
}

function rawX25519Keys(): { pub: Buffer; priv: Buffer } {
  const kp = crypto.generateKeyPairSync("x25519");
  type Jwk = { x?: string; d?: string };
  const pub = kp.publicKey.export({ format: "jwk" }) as Jwk;
  const priv = kp.privateKey.export({ format: "jwk" }) as Jwk;
  if (!pub.x || !priv.d) {
    throw new Error("X25519 JWK export missing x/d field");
  }
  return { pub: b64urlToBuffer(pub.x), priv: b64urlToBuffer(priv.d) };
}

function buildFacade(data: IdentityData): MeshIdentity {
  const signingPublicKey = rawFromPrefixedB64(data.signing_public_key, "ed25519:");
  return {
    amid: deriveAmid(signingPublicKey),
    did: deriveCanonicalDid(signingPublicKey),
    signingPublicKey,
    signingPrivateKey: rawFromPrefixedB64(data.signing_private_key, "ed25519:"),
  };
}

// ---------------------------------------------------------------------------
// Generate / Load / Save
// ---------------------------------------------------------------------------

async function writeEnvelope(data: IdentityData): Promise<void> {
  fs.mkdirSync(IDENTITY_DIR, { recursive: true });
  const { ciphertext, iv, authTag } = encryptJson(JSON.stringify(data));
  const envelope: MeshIdentityEnvelope = {
    schema: 2,
    ciphertext,
    iv,
    authTag,
    createdAt: new Date().toISOString(),
  };
  fs.writeFileSync(IDENTITY_FILE, JSON.stringify(envelope, null, 2), { mode: 0o600 });
}

/** Generate a fresh identity (Ed25519 signing + X25519 exchange) and persist it. */
export async function generateIdentity(): Promise<MeshIdentity> {
  const sig = rawEd25519Keys();
  const exch = rawX25519Keys();
  const data: IdentityData = {
    signing_public_key: `ed25519:${sig.pub.toString("base64")}`,
    signing_private_key: `ed25519:${sig.priv.toString("base64")}`,
    exchange_public_key: `x25519:${exch.pub.toString("base64")}`,
    exchange_private_key: `x25519:${exch.priv.toString("base64")}`,
  };
  await writeEnvelope(data);
  return buildFacade(data);
}

/**
 * Persist identity to disk. Kept for API compatibility — `generateIdentity()`
 * already writes on creation, so this is a no-op for in-memory facades.
 */
export async function saveIdentity(_identity: MeshIdentity): Promise<void> {
  // intentionally empty: generation already persisted the envelope.
}

/**
 * Load identity from disk. Returns `null` if the file is missing, corrupt,
 * or written in a legacy (pre-X25519) format. Callers should fall back to
 * `generateIdentity()` in that case.
 */
export async function loadIdentity(): Promise<MeshIdentity | null> {
  if (!fs.existsSync(IDENTITY_FILE)) return null;
  try {
    const raw = fs.readFileSync(IDENTITY_FILE, "utf-8");
    const parsed = JSON.parse(raw) as Partial<MeshIdentityEnvelope>;
    if (parsed.schema !== 2 || !parsed.ciphertext || !parsed.iv || !parsed.authTag) {
      return null;
    }
    const json = decryptJson(parsed as MeshIdentityEnvelope);
    const data = JSON.parse(json) as Partial<IdentityData>;
    if (
      !data.signing_public_key ||
      !data.signing_private_key ||
      !data.exchange_public_key ||
      !data.exchange_private_key
    ) {
      return null;
    }
    return buildFacade(data as IdentityData);
  } catch {
    return null;
  }
}

/**
 * Load the existing identity or generate a fresh one. A legacy identity file
 * (schema < 2) is treated as absent and replaced.
 */
export async function loadOrCreateIdentity(): Promise<MeshIdentity> {
  const existing = await loadIdentity();
  if (existing) return existing;
  return generateIdentity();
}

/** Absolute path of the on-disk identity file (for display / diagnostics). */
export function getIdentityPath(): string {
  return IDENTITY_FILE;
}

/**
 * Verify an Ed25519 signature using Node.js native crypto.
 *
 * Accepts the public key as either a base64 string (with or without
 * `ed25519:` prefix) or a raw 32-byte Buffer/Uint8Array. The signature
 * accepts the same shapes (raw 64 bytes, or base64 string).
 *
 * Returns `true` on a valid signature, `false` on any failure (including
 * malformed inputs). Never throws.
 */
export function verifyEd25519Signature(
  publicKey: string | Uint8Array | Buffer,
  data: Uint8Array | Buffer,
  signature: string | Uint8Array | Buffer,
): boolean {
  try {
    const pubRaw =
      typeof publicKey === "string"
        ? Buffer.from(publicKey.startsWith("ed25519:") ? publicKey.slice(8) : publicKey, "base64")
        : Buffer.from(publicKey);
    const sigRaw =
      typeof signature === "string" ? Buffer.from(signature, "base64") : Buffer.from(signature);
    if (pubRaw.length !== 32 || sigRaw.length !== 64) return false;
    const pubKeyObj = crypto.createPublicKey({
      key: { kty: "OKP", crv: "Ed25519", x: pubRaw.toString("base64url") },
      format: "jwk",
    });
    return crypto.verify(null, Buffer.from(data), pubKeyObj, sigRaw);
  } catch {
    return false;
  }
}
