/**
 * Mesh identity management — Ed25519 signing + X25519 exchange keypairs.
 *
 * Identity is owned by `@agentmesh/sdk` (full Signal-capable `Identity` class).
 * We persist the SDK's `IdentityData` JSON at ~/.azureclaw/identity.json,
 * encrypted at rest with AES-256-GCM (key derived from hostname+homedir).
 *
 * Exposes a `MeshIdentity` facade so the rest of the plugin sees a stable
 * shape (amid + raw signing keys). The SDK `Identity` object is carried
 * alongside for direct use with `AgentMeshClient.fromIdentity()`.
 */

import * as crypto from "node:crypto";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

import { Identity, type IdentityData } from "@agentmesh/sdk";

const IDENTITY_DIR = path.join(os.homedir(), ".azureclaw");
const IDENTITY_FILE = path.join(IDENTITY_DIR, "identity.json");

/** Envelope written to disk — SDK IdentityData encrypted with AES-256-GCM. */
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
  /** Ed25519 signing public key (raw 32 bytes). */
  signingPublicKey: Buffer;
  /** Ed25519 signing private key (raw 32 bytes). */
  signingPrivateKey: Buffer;
  /** The SDK-native Identity object, used by AgentMeshClient.fromIdentity(). */
  sdkIdentity: Identity;
}

// ---------------------------------------------------------------------------
// AMID derivation — matches the SDK: base58(sha256(pubkey)[:20]).
// Kept exported for legacy callers; the SDK derives the same value.
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
// SDK key extraction — "ed25519:<base64>" / "x25519:<base64>" → Buffer(raw)
// ---------------------------------------------------------------------------

function stripPrefix(value: string, prefix: string): string {
  return value.startsWith(prefix) ? value.slice(prefix.length) : value;
}

function rawFromPrefixedB64(value: string, prefix: string): Buffer {
  return Buffer.from(stripPrefix(value, prefix), "base64");
}

// ---------------------------------------------------------------------------
// Build MeshIdentity from an SDK Identity
// ---------------------------------------------------------------------------

async function buildFacade(sdkIdentity: Identity): Promise<MeshIdentity> {
  const data = await sdkIdentity.toData();
  return {
    amid: sdkIdentity.amid,
    signingPublicKey: rawFromPrefixedB64(data.signing_public_key, "ed25519:"),
    signingPrivateKey: rawFromPrefixedB64(data.signing_private_key, "ed25519:"),
    sdkIdentity,
  };
}

// ---------------------------------------------------------------------------
// Generate / Load / Save
// ---------------------------------------------------------------------------

/** Generate a fresh identity (Ed25519 signing + X25519 exchange) via the SDK. */
export async function generateIdentity(): Promise<MeshIdentity> {
  const sdkIdentity = await Identity.generate();
  return buildFacade(sdkIdentity);
}

/** Persist identity to ~/.azureclaw/identity.json (encrypted envelope). */
export async function saveIdentity(identity: MeshIdentity): Promise<void> {
  fs.mkdirSync(IDENTITY_DIR, { recursive: true });
  const data = await identity.sdkIdentity.toData();
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
    // Reject legacy Ed25519-only envelopes — we require X25519 too for Signal.
    if (parsed.schema !== 2 || !parsed.ciphertext || !parsed.iv || !parsed.authTag) {
      return null;
    }
    const json = decryptJson(parsed as MeshIdentityEnvelope);
    const data = JSON.parse(json) as IdentityData;
    if (!data.exchange_private_key || !data.exchange_public_key) return null;
    const sdkIdentity = await Identity.fromData(data);
    return await buildFacade(sdkIdentity);
  } catch {
    return null;
  }
}

/**
 * Load the existing identity or generate a fresh one. A legacy identity file
 * (Ed25519-only, schema < 2) is treated as absent and replaced — the Signal
 * upgrade needs X25519 exchange keys that the legacy format never stored.
 */
export async function loadOrCreateIdentity(): Promise<MeshIdentity> {
  const existing = await loadIdentity();
  if (existing) return existing;
  const identity = await generateIdentity();
  await saveIdentity(identity);
  return identity;
}

/** Absolute path of the on-disk identity file (for display / diagnostics). */
export function getIdentityPath(): string {
  return IDENTITY_FILE;
}

