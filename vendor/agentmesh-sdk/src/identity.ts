/**
 * Identity management for AgentMesh agents.
 * Handles key generation, AMID derivation, and signatures.
 */

import type { Storage } from './storage/interface';
import { StorageNamespace, namespacedKey } from './storage/interface';
import { CryptoError } from './errors';

/**
 * Helper to convert Uint8Array to BufferSource for Web Crypto API.
 * This ensures proper type compatibility with strict TypeScript.
 */
function toBufferSource(data: Uint8Array): BufferSource {
  return data as unknown as BufferSource;
}

/**
 * Helper to create a copy of Uint8Array as proper ArrayBuffer.
 * Needed because some Web Crypto operations are picky about buffer types.
 */
function toArrayBuffer(data: Uint8Array): ArrayBuffer {
  // Create a copy in a fresh ArrayBuffer
  const buffer = new ArrayBuffer(data.length);
  new Uint8Array(buffer).set(data);
  return buffer;
}

// Base58 alphabet (Bitcoin-style)
const BASE58_ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';

/**
 * Encode bytes to Base58 string.
 */
function encodeBase58(bytes: Uint8Array): string {
  // Convert bytes to big integer
  let num = 0n;
  for (const byte of bytes) {
    num = num * 256n + BigInt(byte);
  }

  // Convert to base58
  const result: string[] = [];
  while (num > 0n) {
    const remainder = Number(num % 58n);
    result.push(BASE58_ALPHABET[remainder]!);
    num = num / 58n;
  }

  // Add leading zeros
  for (const byte of bytes) {
    if (byte === 0) {
      result.push(BASE58_ALPHABET[0]!);
    } else {
      break;
    }
  }

  return result.reverse().join('');
}

/**
 * Derive AgentMesh ID from signing public key.
 * AMID = base58(sha256(public_key)[:20])
 */
async function deriveAmid(signingPublicKey: Uint8Array): Promise<string> {
  const hashBuffer = await crypto.subtle.digest('SHA-256', toBufferSource(signingPublicKey));
  const hashBytes = new Uint8Array(hashBuffer).slice(0, 20);
  return encodeBase58(hashBytes);
}

/**
 * Convert ArrayBuffer or Uint8Array to base64 string.
 */
function arrayBufferToBase64(buffer: ArrayBuffer | Uint8Array): string {
  const bytes = buffer instanceof Uint8Array ? buffer : new Uint8Array(buffer);
  let binary = '';
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary);
}

/**
 * Convert base64 string to Uint8Array.
 */
function base64ToUint8Array(base64: string): Uint8Array {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

/**
 * Strip key type prefix if present.
 */
function stripKeyPrefix(keyStr: string, expectedPrefix: string): string {
  if (keyStr.startsWith(expectedPrefix)) {
    return keyStr.slice(expectedPrefix.length);
  }
  if (keyStr.includes(':')) {
    const [, rest] = keyStr.split(':', 2);
    console.warn(`Key has unexpected prefix, expected '${expectedPrefix}'`);
    return rest ?? keyStr;
  }
  console.warn(`Key without type prefix detected. Keys should be prefixed with '${expectedPrefix}'`);
  return keyStr;
}

/**
 * Serialized identity data for storage.
 */
export interface IdentityData {
  amid: string;
  signing_private_key: string;
  signing_public_key: string;
  exchange_private_key: string;
  exchange_public_key: string;
  created_at: string;
  framework: string;
  framework_version: string;
}

/**
 * Public information for registration/discovery.
 */
export interface PublicInfo {
  amid: string;
  signing_public_key: string;
  exchange_public_key: string;
  framework: string;
  framework_version: string;
}

/**
 * Cryptographic identity for an AgentMesh agent.
 */
export class Identity {
  /** AgentMesh ID - derived from signing public key */
  readonly amid: string;

  /** Ed25519 signing key pair */
  private readonly signingPrivateKey: CryptoKey;
  private readonly signingPublicKey: CryptoKey;
  private readonly signingPublicKeyRaw: Uint8Array;

  /** X25519 key exchange key pair */
  private readonly exchangePrivateKey: CryptoKey;
  private readonly exchangePublicKey: CryptoKey;
  private readonly exchangePublicKeyRaw: Uint8Array;
  private readonly exchangePrivateKeyRaw: Uint8Array;

  /** Metadata */
  readonly createdAt: Date;
  readonly framework: string;
  readonly frameworkVersion: string;

  private constructor(
    amid: string,
    signingPrivateKey: CryptoKey,
    signingPublicKey: CryptoKey,
    signingPublicKeyRaw: Uint8Array,
    exchangePrivateKey: CryptoKey,
    exchangePublicKey: CryptoKey,
    exchangePublicKeyRaw: Uint8Array,
    exchangePrivateKeyRaw: Uint8Array,
    createdAt: Date,
    framework: string,
    frameworkVersion: string
  ) {
    this.amid = amid;
    this.signingPrivateKey = signingPrivateKey;
    this.signingPublicKey = signingPublicKey;
    this.signingPublicKeyRaw = signingPublicKeyRaw;
    this.exchangePrivateKey = exchangePrivateKey;
    this.exchangePublicKey = exchangePublicKey;
    this.exchangePublicKeyRaw = exchangePublicKeyRaw;
    this.exchangePrivateKeyRaw = exchangePrivateKeyRaw;
    this.createdAt = createdAt;
    this.framework = framework;
    this.frameworkVersion = frameworkVersion;
  }

  /**
   * Generate a new cryptographic identity.
   */
  static async generate(): Promise<Identity> {
    try {
      // Generate Ed25519 signing key pair
      const signingKeyPair = (await crypto.subtle.generateKey(
        { name: 'Ed25519' } as Algorithm,
        true, // extractable
        ['sign', 'verify']
      )) as CryptoKeyPair;

      // Generate X25519 key exchange key pair
      const exchangeKeyPair = (await crypto.subtle.generateKey(
        { name: 'X25519' } as Algorithm,
        true, // extractable
        ['deriveBits']
      )) as CryptoKeyPair;

      // Export public keys as raw bytes
      const signingPublicKeyRaw = new Uint8Array(
        await crypto.subtle.exportKey('raw', signingKeyPair.publicKey)
      );
      const exchangePublicKeyRaw = new Uint8Array(
        await crypto.subtle.exportKey('raw', exchangeKeyPair.publicKey)
      );

      // Export exchange private key as raw bytes (for X3DH)
      const exchangePrivateKeyPkcs8 = await crypto.subtle.exportKey('pkcs8', exchangeKeyPair.privateKey);
      const exchangePrivateKeyRaw = unwrapX25519PrivateKeyPKCS8(new Uint8Array(exchangePrivateKeyPkcs8));

      // Derive AMID from signing public key
      const amid = await deriveAmid(signingPublicKeyRaw);

      return new Identity(
        amid,
        signingKeyPair.privateKey,
        signingKeyPair.publicKey,
        signingPublicKeyRaw,
        exchangeKeyPair.privateKey,
        exchangeKeyPair.publicKey,
        exchangePublicKeyRaw,
        exchangePrivateKeyRaw,
        new Date(),
        'agentmesh-js',
        '0.1.0'
      );
    } catch (error) {
      throw new CryptoError(
        `Failed to generate identity: ${error instanceof Error ? error.message : 'Unknown error'}`,
        'KEY_GENERATION_FAILED'
      );
    }
  }

  /**
   * Load identity from storage.
   */
  static async load(storage: Storage, path: string): Promise<Identity> {
    const key = namespacedKey(StorageNamespace.IDENTITY, path);
    const data = await storage.get(key);

    if (!data) {
      throw new CryptoError(`Identity not found at ${path}`, 'IDENTITY_NOT_FOUND');
    }

    try {
      const json = new TextDecoder().decode(data);
      const parsed = JSON.parse(json) as IdentityData;
      return Identity.fromData(parsed);
    } catch (error) {
      throw new CryptoError(
        `Failed to load identity: ${error instanceof Error ? error.message : 'Unknown error'}`,
        'IDENTITY_LOAD_FAILED'
      );
    }
  }

  /**
   * Create identity from serialized data.
   */
  static async fromData(data: IdentityData): Promise<Identity> {
    try {
      // Parse and import signing keys
      const signingPrivateKeyB64 = stripKeyPrefix(data.signing_private_key, 'ed25519:');
      const signingPublicKeyB64 = stripKeyPrefix(data.signing_public_key, 'ed25519:');

      const signingPrivateKeyRaw = base64ToUint8Array(signingPrivateKeyB64);
      const signingPublicKeyRaw = base64ToUint8Array(signingPublicKeyB64);

      // Import Ed25519 keys using PKCS#8 for private key
      // Web Crypto requires PKCS8 format for Ed25519 private keys
      const signingKeyPair = await importEd25519KeyPair(signingPrivateKeyRaw, signingPublicKeyRaw);

      // Parse and import exchange keys
      const exchangePrivateKeyB64 = stripKeyPrefix(data.exchange_private_key, 'x25519:');
      const exchangePublicKeyB64 = stripKeyPrefix(data.exchange_public_key, 'x25519:');

      const exchangePrivateKeyRaw = base64ToUint8Array(exchangePrivateKeyB64);
      const exchangePublicKeyRaw = base64ToUint8Array(exchangePublicKeyB64);

      // Import X25519 keys
      const exchangeKeyPair = await importX25519KeyPair(exchangePrivateKeyRaw, exchangePublicKeyRaw);

      return new Identity(
        data.amid,
        signingKeyPair.privateKey,
        signingKeyPair.publicKey,
        signingPublicKeyRaw,
        exchangeKeyPair.privateKey,
        exchangeKeyPair.publicKey,
        exchangePublicKeyRaw,
        exchangePrivateKeyRaw,
        new Date(data.created_at),
        data.framework,
        data.framework_version
      );
    } catch (error) {
      throw new CryptoError(
        `Failed to import identity: ${error instanceof Error ? error.message : 'Unknown error'}`,
        'IDENTITY_IMPORT_FAILED'
      );
    }
  }

  /**
   * Save identity to storage.
   */
  async save(storage: Storage, path: string): Promise<void> {
    const data = await this.toData();
    const json = JSON.stringify(data, null, 2);
    const bytes = new TextEncoder().encode(json);

    const key = namespacedKey(StorageNamespace.IDENTITY, path);
    await storage.set(key, bytes);
  }

  /**
   * Export identity as serializable data.
   */
  async toData(): Promise<IdentityData> {
    // Export private keys
    const signingPrivateKeyRaw = await exportEd25519PrivateKey(this.signingPrivateKey);
    const exchangePrivateKeyRaw = await exportX25519PrivateKey(this.exchangePrivateKey);

    return {
      amid: this.amid,
      signing_private_key: 'ed25519:' + arrayBufferToBase64(signingPrivateKeyRaw),
      signing_public_key: 'ed25519:' + arrayBufferToBase64(this.signingPublicKeyRaw),
      exchange_private_key: 'x25519:' + arrayBufferToBase64(exchangePrivateKeyRaw),
      exchange_public_key: 'x25519:' + arrayBufferToBase64(this.exchangePublicKeyRaw),
      created_at: this.createdAt.toISOString(),
      framework: this.framework,
      framework_version: this.frameworkVersion,
    };
  }

  /**
   * Get signing public key as base64 with type prefix.
   */
  get signingPublicKeyB64(): string {
    return 'ed25519:' + arrayBufferToBase64(this.signingPublicKeyRaw);
  }

  /**
   * Get signing public key as base64 without prefix (for signature verification).
   */
  get signingPublicKeyB64Raw(): string {
    return arrayBufferToBase64(this.signingPublicKeyRaw);
  }

  /**
   * Get exchange public key as base64 with type prefix.
   */
  get exchangePublicKeyB64(): string {
    return 'x25519:' + arrayBufferToBase64(this.exchangePublicKeyRaw);
  }

  /**
   * Get exchange public key as base64 without prefix.
   */
  get exchangePublicKeyB64Raw(): string {
    return arrayBufferToBase64(this.exchangePublicKeyRaw);
  }

  /**
   * Get raw signing public key bytes.
   */
  getSigningPublicKeyRaw(): Uint8Array {
    return new Uint8Array(this.signingPublicKeyRaw);
  }

  /**
   * Get raw exchange public key bytes.
   */
  getExchangePublicKeyRaw(): Uint8Array {
    return new Uint8Array(this.exchangePublicKeyRaw);
  }

  /**
   * Get the exchange private key as CryptoKey (for DH operations).
   */
  getExchangePrivateKey(): CryptoKey {
    return this.exchangePrivateKey;
  }

  /**
   * Get the exchange private key as raw bytes (for X3DH).
   */
  getExchangePrivateKeyRaw(): Uint8Array {
    return new Uint8Array(this.exchangePrivateKeyRaw);
  }

  /**
   * Sign a message with the signing key.
   */
  async sign(message: Uint8Array): Promise<Uint8Array> {
    try {
      const signature = await crypto.subtle.sign(
        { name: 'Ed25519' } as Algorithm,
        this.signingPrivateKey,
        toBufferSource(message)
      );
      return new Uint8Array(signature);
    } catch (error) {
      throw new CryptoError(
        `Failed to sign message: ${error instanceof Error ? error.message : 'Unknown error'}`,
        'SIGN_FAILED'
      );
    }
  }

  /**
   * Sign a message and return base64-encoded signature.
   */
  async signB64(message: Uint8Array): Promise<string> {
    const signature = await this.sign(message);
    return arrayBufferToBase64(signature);
  }

  /**
   * Sign the current timestamp for authentication.
   */
  async signTimestamp(): Promise<[string, string]> {
    const timestamp = new Date().toISOString();
    const message = new TextEncoder().encode(timestamp);
    const signature = await this.signB64(message);
    return [timestamp, signature];
  }

  /**
   * Verify a signature from another agent.
   */
  static async verifySignature(
    publicKeyB64: string,
    message: Uint8Array,
    signatureB64: string
  ): Promise<boolean> {
    try {
      // Strip prefix if present
      let keyB64 = publicKeyB64;
      if (keyB64.startsWith('ed25519:')) {
        keyB64 = keyB64.slice(8);
      } else if (keyB64.startsWith('x25519:')) {
        keyB64 = keyB64.slice(7);
      }

      const publicKeyRaw = base64ToUint8Array(keyB64);
      const signature = base64ToUint8Array(signatureB64);

      // Import public key
      const publicKey = await crypto.subtle.importKey(
        'raw',
        toArrayBuffer(publicKeyRaw),
        { name: 'Ed25519' } as Algorithm,
        false,
        ['verify']
      );

      return await crypto.subtle.verify(
        { name: 'Ed25519' } as Algorithm,
        publicKey,
        toBufferSource(signature),
        toBufferSource(message)
      );
    } catch {
      return false;
    }
  }

  /**
   * Verify a signature using raw bytes (for X3DH).
   */
  static async verifySignatureRaw(
    publicKeyRaw: Uint8Array,
    message: Uint8Array,
    signatureRaw: Uint8Array
  ): Promise<boolean> {
    try {
      // Import public key
      const publicKey = await crypto.subtle.importKey(
        'raw',
        toArrayBuffer(publicKeyRaw),
        { name: 'Ed25519' } as Algorithm,
        false,
        ['verify']
      );

      return await crypto.subtle.verify(
        { name: 'Ed25519' } as Algorithm,
        publicKey,
        toBufferSource(signatureRaw),
        toBufferSource(message)
      );
    } catch {
      return false;
    }
  }

  /**
   * Get public information for registration/discovery.
   */
  toPublicInfo(): PublicInfo {
    return {
      amid: this.amid,
      signing_public_key: this.signingPublicKeyB64,
      exchange_public_key: this.exchangePublicKeyB64,
      framework: this.framework,
      framework_version: this.frameworkVersion,
    };
  }

  /**
   * Derive a secret from the identity using HMAC-SHA256.
   * Used for deriving application-specific keys.
   */
  async deriveSecret(info: string): Promise<Uint8Array> {
    // Export the exchange private key as our master secret
    const masterSecret = this.exchangePrivateKeyRaw;

    // Import as HMAC key
    const hmacKey = await crypto.subtle.importKey(
      'raw',
      toArrayBuffer(masterSecret),
      { name: 'HMAC', hash: 'SHA-256' },
      false,
      ['sign']
    );

    // Derive secret = HMAC(master, info)
    const infoBytes = new TextEncoder().encode(info);
    const derived = await crypto.subtle.sign('HMAC', hmacKey, toBufferSource(infoBytes));

    return new Uint8Array(derived);
  }

  /**
   * Rotate cryptographic keys.
   * Returns a new Identity with fresh keys.
   */
  async rotateKeys(): Promise<Identity> {
    console.warn(`Key rotation: ${this.amid} will be replaced with new identity`);
    return Identity.generate();
  }
}

/**
 * Import Ed25519 key pair from raw bytes.
 * Note: Web Crypto Ed25519 requires special handling.
 */
async function importEd25519KeyPair(
  privateKeyRaw: Uint8Array,
  publicKeyRaw: Uint8Array
): Promise<{ privateKey: CryptoKey; publicKey: CryptoKey }> {
  // Import public key (raw format works directly)
  // Ed25519 and X25519 are newer algorithms, need type assertions
  const publicKey = await crypto.subtle.importKey(
    'raw',
    toArrayBuffer(publicKeyRaw),
    { name: 'Ed25519' } as Algorithm,
    true,
    ['verify']
  );

  // For Ed25519 private key, Web Crypto expects PKCS8 format
  // We need to wrap the raw 32-byte seed in PKCS8 structure
  const pkcs8 = wrapEd25519PrivateKeyPKCS8(privateKeyRaw);
  const privateKey = await crypto.subtle.importKey(
    'pkcs8',
    toArrayBuffer(pkcs8),
    { name: 'Ed25519' } as Algorithm,
    true,
    ['sign']
  );

  return { privateKey, publicKey };
}

/**
 * Import X25519 key pair from raw bytes.
 */
async function importX25519KeyPair(
  privateKeyRaw: Uint8Array,
  publicKeyRaw: Uint8Array
): Promise<{ privateKey: CryptoKey; publicKey: CryptoKey }> {
  // Import public key
  const publicKey = await crypto.subtle.importKey(
    'raw',
    toArrayBuffer(publicKeyRaw),
    { name: 'X25519' } as Algorithm,
    true,
    []
  );

  // For X25519 private key, use PKCS8 format
  const pkcs8 = wrapX25519PrivateKeyPKCS8(privateKeyRaw);
  const privateKey = await crypto.subtle.importKey(
    'pkcs8',
    toArrayBuffer(pkcs8),
    { name: 'X25519' } as Algorithm,
    true,
    ['deriveBits']
  );

  return { privateKey, publicKey };
}

/**
 * Export Ed25519 private key as raw bytes.
 */
async function exportEd25519PrivateKey(key: CryptoKey): Promise<Uint8Array> {
  const pkcs8 = await crypto.subtle.exportKey('pkcs8', key);
  return unwrapEd25519PrivateKeyPKCS8(new Uint8Array(pkcs8));
}

/**
 * Export X25519 private key as raw bytes.
 */
async function exportX25519PrivateKey(key: CryptoKey): Promise<Uint8Array> {
  const pkcs8 = await crypto.subtle.exportKey('pkcs8', key);
  return unwrapX25519PrivateKeyPKCS8(new Uint8Array(pkcs8));
}

// PKCS8 wrapper for Ed25519 private key
// Structure: SEQUENCE { version, algorithm, privateKey }
const ED25519_PKCS8_PREFIX = new Uint8Array([
  0x30, 0x2e, // SEQUENCE, length 46
  0x02, 0x01, 0x00, // INTEGER version = 0
  0x30, 0x05, // SEQUENCE algorithm
  0x06, 0x03, 0x2b, 0x65, 0x70, // OID 1.3.101.112 (Ed25519)
  0x04, 0x22, // OCTET STRING, length 34
  0x04, 0x20, // OCTET STRING (inner), length 32
]);

function wrapEd25519PrivateKeyPKCS8(rawKey: Uint8Array): Uint8Array {
  const result = new Uint8Array(ED25519_PKCS8_PREFIX.length + rawKey.length);
  result.set(ED25519_PKCS8_PREFIX);
  result.set(rawKey, ED25519_PKCS8_PREFIX.length);
  return result;
}

function unwrapEd25519PrivateKeyPKCS8(pkcs8: Uint8Array): Uint8Array {
  // The raw key is the last 32 bytes
  return pkcs8.slice(-32);
}

// PKCS8 wrapper for X25519 private key
const X25519_PKCS8_PREFIX = new Uint8Array([
  0x30, 0x2e, // SEQUENCE, length 46
  0x02, 0x01, 0x00, // INTEGER version = 0
  0x30, 0x05, // SEQUENCE algorithm
  0x06, 0x03, 0x2b, 0x65, 0x6e, // OID 1.3.101.110 (X25519)
  0x04, 0x22, // OCTET STRING, length 34
  0x04, 0x20, // OCTET STRING (inner), length 32
]);

function wrapX25519PrivateKeyPKCS8(rawKey: Uint8Array): Uint8Array {
  const result = new Uint8Array(X25519_PKCS8_PREFIX.length + rawKey.length);
  result.set(X25519_PKCS8_PREFIX);
  result.set(rawKey, X25519_PKCS8_PREFIX.length);
  return result;
}

function unwrapX25519PrivateKeyPKCS8(pkcs8: Uint8Array): Uint8Array {
  // The raw key is the last 32 bytes
  return pkcs8.slice(-32);
}
