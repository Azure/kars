/**
 * AGT-compatible identity — generates Ed25519 keys using Node.js crypto
 * and derives the agent DID per AgentMesh Wire Protocol v1.0 Section 4.
 *
 * Replaces the @agentmesh/sdk Identity dependency. Uses only Node.js
 * built-in crypto (no external dependencies).
 *
 * Identity format: did:agentmesh:<fingerprint>
 * where fingerprint = base58btc(sha256(ed25519_public_key)[0:20])
 */

import * as crypto from "node:crypto";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import type { IMeshIdentity } from "./transport-interface.js";

const IDENTITY_DIR = path.join(os.homedir(), ".azureclaw");
const IDENTITY_FILE = path.join(IDENTITY_DIR, "mesh-identity-agt.json");

const BASE58_ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

function encodeBase58(bytes: Uint8Array): string {
  let num = BigInt(0);
  for (const b of bytes) num = num * BigInt(256) + BigInt(b);
  if (num === BigInt(0)) return BASE58_ALPHABET[0];
  let result = "";
  while (num > BigInt(0)) {
    result = BASE58_ALPHABET[Number(num % BigInt(58))] + result;
    num = num / BigInt(58);
  }
  for (const b of bytes) {
    if (b === 0) result = BASE58_ALPHABET[0] + result;
    else break;
  }
  return result;
}

export function deriveDid(signingPublicKey: Uint8Array): string {
  const hash = crypto.createHash("sha256").update(signingPublicKey).digest();
  const fingerprint = encodeBase58(hash.subarray(0, 20));
  return `did:agentmesh:${fingerprint}`;
}

/** Also export AMID derivation for backward compatibility with existing relay/registry */
export function deriveAmid(signingPublicKey: Uint8Array): string {
  const hash = crypto.createHash("sha256").update(signingPublicKey).digest();
  return encodeBase58(hash.subarray(0, 20));
}

export interface AgtMeshIdentity extends IMeshIdentity {
  /** Legacy AMID (for backward compat with existing relay) */
  amid: string;
}

interface IdentityEnvelope {
  schema: 3;
  signingPublicKey: string; // hex
  signingPrivateKey: string; // hex, encrypted
  iv: string;
  authTag: string;
  did: string;
  amid: string;
  createdAt: string;
}

function deriveEncryptionKey(): Buffer {
  const seed = `azureclaw:agt-mesh-identity:${os.hostname()}:${os.homedir()}`;
  return crypto.createHash("sha256").update(seed).digest();
}

export function generateIdentity(): AgtMeshIdentity {
  const keyPair = crypto.generateKeyPairSync("ed25519");
  const publicKeyDer = keyPair.publicKey.export({ type: "spki", format: "der" });
  const privateKeyDer = keyPair.privateKey.export({ type: "pkcs8", format: "der" });

  // Ed25519 raw key is the last 32 bytes of DER encoding
  const signingPublicKey = new Uint8Array(publicKeyDer.subarray(publicKeyDer.length - 32));
  const signingPrivateKey = new Uint8Array(privateKeyDer.subarray(privateKeyDer.length - 32));

  const did = deriveDid(signingPublicKey);
  const amid = deriveAmid(signingPublicKey);

  return { agentId: did, signingPublicKey, signingPrivateKey, amid };
}

export function saveIdentity(identity: AgtMeshIdentity): void {
  fs.mkdirSync(IDENTITY_DIR, { recursive: true });

  const key = deriveEncryptionKey();
  const iv = crypto.randomBytes(12);
  const cipher = crypto.createCipheriv("aes-256-gcm", key, iv);
  const encrypted = Buffer.concat([
    cipher.update(Buffer.from(identity.signingPrivateKey)),
    cipher.final(),
  ]);

  const envelope: IdentityEnvelope = {
    schema: 3,
    signingPublicKey: Buffer.from(identity.signingPublicKey).toString("hex"),
    signingPrivateKey: encrypted.toString("hex"),
    iv: iv.toString("hex"),
    authTag: cipher.getAuthTag().toString("hex"),
    did: identity.agentId,
    amid: identity.amid,
    createdAt: new Date().toISOString(),
  };

  fs.writeFileSync(IDENTITY_FILE, JSON.stringify(envelope, null, 2), { mode: 0o600 });
}

export function loadIdentity(): AgtMeshIdentity | null {
  if (!fs.existsSync(IDENTITY_FILE)) return null;
  try {
    const raw = JSON.parse(fs.readFileSync(IDENTITY_FILE, "utf-8")) as Partial<IdentityEnvelope>;
    if (raw.schema !== 3 || !raw.signingPublicKey || !raw.signingPrivateKey) return null;

    const key = deriveEncryptionKey();
    const decipher = crypto.createDecipheriv(
      "aes-256-gcm", key, Buffer.from(raw.iv!, "hex"),
    );
    decipher.setAuthTag(Buffer.from(raw.authTag!, "hex"));
    const decrypted = Buffer.concat([
      decipher.update(Buffer.from(raw.signingPrivateKey, "hex")),
      decipher.final(),
    ]);

    const signingPublicKey = new Uint8Array(Buffer.from(raw.signingPublicKey, "hex"));
    const signingPrivateKey = new Uint8Array(decrypted);

    return {
      agentId: raw.did!,
      signingPublicKey,
      signingPrivateKey,
      amid: raw.amid!,
    };
  } catch {
    return null;
  }
}

export function loadOrCreateIdentity(): AgtMeshIdentity {
  const existing = loadIdentity();
  if (existing) return existing;
  const identity = generateIdentity();
  saveIdentity(identity);
  return identity;
}

export function getIdentityPath(): string {
  return IDENTITY_FILE;
}
