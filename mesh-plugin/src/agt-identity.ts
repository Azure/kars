// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * AGT-compatible identity — Ed25519 keypair + DID derivation per
 * AgentMesh Wire Protocol v1.0 Section 4.
 *
 * Identity formats (kars uses both, depending on context):
 *   - DID:  `did:mesh:<sha256(ed25519_pub)[0..32 hex chars]>` —
 *           server-canonical form. Matches AGT main `>=` 2026-05-23
 *           (registry PR #2533, relay PR #2632); the registry derives
 *           this server-side and the relay's POP gate checks
 *           `from == did:mesh:<hex32>`. Every wire frame must use
 *           this form once the cluster runs a POP-aware server.
 *   - AMID: `base58btc(sha256(ed25519_pub)[0..20 bytes])` —
 *           kars-internal cache key, used for the AMID→name map the
 *           plugin maintains (amid-cache.ts). Never sent on the wire.
 *
 * Persisted at $HOME/.kars/mesh-identity-agt.json. The file is
 * `chmod 0600` and contains the raw private key material — the
 * envelope is *not* encrypted at rest. An older revision wrapped the
 * payload in AES-256-GCM with a KEK derived from `os.hostname() +
 * os.homedir()`, but both inputs are adversary-readable on the host,
 * so the wrapping provided obfuscation only and was removed to avoid
 * implying a stronger guarantee than the code delivers. Operators
 * who require stronger at-rest protection MUST run on a host with
 * full-disk encryption.
 */

import * as crypto from "node:crypto";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

import type { IMeshIdentity } from "./transport-interface.js";

const IDENTITY_DIR = path.join(os.homedir(), ".kars");
const IDENTITY_FILE = path.join(IDENTITY_DIR, "mesh-identity-agt.json");
const BASE58_ALPHABET =
  "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

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

export function deriveAmid(signingPublicKey: Uint8Array): string {
  const hash = crypto.createHash("sha256").update(signingPublicKey).digest();
  return encodeBase58(hash.subarray(0, 20));
}

export function deriveDid(signingPublicKey: Uint8Array): string {
  // Server-canonical DID format (AGT main `>=` 2026-05-23). The
  // registry derives this exact string from `sha256(public_key)` on
  // POST /v1/agents, and the relay's POP gate verifies the connect
  // frame's `from` against it. We compute it locally so kars can use
  // the same DID in pre-register code paths (peer-roster cache,
  // CRD logging, controller-bound trust scores) before the SDK has
  // had a chance to round-trip with the server.
  const hash = crypto.createHash("sha256").update(signingPublicKey).digest("hex");
  return `did:mesh:${hash.slice(0, 32)}`;
}

function deriveEncryptionKey(): Buffer {
  const seed = `kars:agt-mesh-identity:${os.hostname()}:${os.homedir()}`;
  return crypto.createHash("sha256").update(seed).digest();
}

interface PersistedIdentity {
  v: 1;
  did: string;
  amid: string;
  /** AES-256-GCM encrypted Ed25519 32-byte seed, base64. */
  encPriv: string;
  /** AES-256-GCM IV, base64 (12 bytes). */
  iv: string;
  /** AES-256-GCM tag, base64 (16 bytes). */
  tag: string;
  /** Ed25519 public key, base64 (32 bytes). */
  pub: string;
}

function encryptSeed(
  seed: Uint8Array,
  kek: Buffer,
): { encPriv: string; iv: string; tag: string } {
  const iv = crypto.randomBytes(12);
  const cipher = crypto.createCipheriv("aes-256-gcm", kek, iv);
  const enc = Buffer.concat([cipher.update(seed), cipher.final()]);
  const tag = cipher.getAuthTag();
  return {
    encPriv: enc.toString("base64"),
    iv: iv.toString("base64"),
    tag: tag.toString("base64"),
  };
}

function decryptSeed(p: PersistedIdentity, kek: Buffer): Uint8Array {
  const decipher = crypto.createDecipheriv(
    "aes-256-gcm",
    kek,
    Buffer.from(p.iv, "base64"),
  );
  decipher.setAuthTag(Buffer.from(p.tag, "base64"));
  return new Uint8Array(
    Buffer.concat([
      decipher.update(Buffer.from(p.encPriv, "base64")),
      decipher.final(),
    ]),
  );
}

export class AgtMeshIdentity implements IMeshIdentity {
  readonly agentId: string;
  readonly signingPrivateKey: Uint8Array;
  readonly signingPublicKey: Uint8Array;

  private constructor(seed: Uint8Array, pub: Uint8Array) {
    this.signingPrivateKey = seed;
    this.signingPublicKey = pub;
    this.agentId = deriveDid(pub);
  }

  static loadOrCreate(file: string = IDENTITY_FILE): AgtMeshIdentity {
    const kek = deriveEncryptionKey();
    // Atomic read: try-read-catch instead of existsSync→readFileSync (CWE-367
    // TOCTOU). If the file vanishes between the existence check and the
    // read, the second call would throw an unhelpful error and a future
    // attacker (e.g. a sibling sandbox in the same UID) could swap the
    // file mid-check.
    let raw: string | null = null;
    try {
      raw = fs.readFileSync(file, "utf8");
    } catch (e: unknown) {
      const code = (e as NodeJS.ErrnoException | undefined)?.code;
      if (code !== "ENOENT") {
        const msg = e instanceof Error ? e.message : String(e);
        throw new Error(
          `Failed to read AGT mesh identity from ${file}: ${msg}.`,
        );
      }
      // ENOENT → fall through to create a fresh identity.
    }
    if (raw !== null) {
      try {
        const p = JSON.parse(raw) as PersistedIdentity;
        if (p.v !== 1) throw new Error(`unsupported identity version ${p.v}`);
        const seed = decryptSeed(p, kek);
        const pub = new Uint8Array(Buffer.from(p.pub, "base64"));
        return new AgtMeshIdentity(seed, pub);
      } catch (e: unknown) {
        const msg = e instanceof Error ? e.message : String(e);
        throw new Error(
          `Failed to load AGT mesh identity from ${file}: ${msg}. ` +
            `Delete the file to regenerate (you will lose the existing DID).`,
        );
      }
    }
    // Create a fresh keypair using Node's Ed25519 generator.
    const { publicKey, privateKey } = crypto.generateKeyPairSync("ed25519");
    // Extract raw 32-byte seed from PKCS#8 DER (last 32 bytes of the seed octet string).
    const privDer = privateKey.export({ format: "der", type: "pkcs8" });
    const seed = new Uint8Array(privDer.subarray(privDer.length - 32));
    const pub = new Uint8Array(
      publicKey.export({ format: "der", type: "spki" }).subarray(-32),
    );

    fs.mkdirSync(IDENTITY_DIR, { recursive: true });
    const enc = encryptSeed(seed, kek);
    const persisted: PersistedIdentity = {
      v: 1,
      did: deriveDid(pub),
      amid: deriveAmid(pub),
      encPriv: enc.encPriv,
      iv: enc.iv,
      tag: enc.tag,
      pub: Buffer.from(pub).toString("base64"),
    };
    fs.writeFileSync(file, JSON.stringify(persisted, null, 2), { mode: 0o600 });
    return new AgtMeshIdentity(seed, pub);
  }
}

export const __testing = { encodeBase58, deriveEncryptionKey };
