'use strict';

var chunkFK3FEKXY_cjs = require('./chunk-FK3FEKXY.cjs');
var chunkFNHOFD2H_cjs = require('./chunk-FNHOFD2H.cjs');

// src/identity.ts
function toBufferSource(data) {
  return data;
}
function toArrayBuffer(data) {
  const buffer = new ArrayBuffer(data.length);
  new Uint8Array(buffer).set(data);
  return buffer;
}
var BASE58_ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
function encodeBase58(bytes) {
  let num = 0n;
  for (const byte of bytes) {
    num = num * 256n + BigInt(byte);
  }
  const result = [];
  while (num > 0n) {
    const remainder = Number(num % 58n);
    result.push(BASE58_ALPHABET[remainder]);
    num = num / 58n;
  }
  for (const byte of bytes) {
    if (byte === 0) {
      result.push(BASE58_ALPHABET[0]);
    } else {
      break;
    }
  }
  return result.reverse().join("");
}
async function deriveAmid(signingPublicKey) {
  const hashBuffer = await crypto.subtle.digest("SHA-256", toBufferSource(signingPublicKey));
  const hashBytes = new Uint8Array(hashBuffer).slice(0, 20);
  return encodeBase58(hashBytes);
}
function arrayBufferToBase64(buffer) {
  const bytes = buffer instanceof Uint8Array ? buffer : new Uint8Array(buffer);
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary);
}
function base64ToUint8Array(base64) {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}
function stripKeyPrefix(keyStr, expectedPrefix) {
  if (keyStr.startsWith(expectedPrefix)) {
    return keyStr.slice(expectedPrefix.length);
  }
  if (keyStr.includes(":")) {
    const [, rest] = keyStr.split(":", 2);
    console.warn(`Key has unexpected prefix, expected '${expectedPrefix}'`);
    return rest ?? keyStr;
  }
  console.warn(`Key without type prefix detected. Keys should be prefixed with '${expectedPrefix}'`);
  return keyStr;
}
var Identity = class _Identity {
  /** AgentMesh ID - derived from signing public key */
  amid;
  /** Ed25519 signing key pair */
  signingPrivateKey;
  signingPublicKey;
  signingPublicKeyRaw;
  /** X25519 key exchange key pair */
  exchangePrivateKey;
  exchangePublicKey;
  exchangePublicKeyRaw;
  exchangePrivateKeyRaw;
  /** Metadata */
  createdAt;
  framework;
  frameworkVersion;
  constructor(amid, signingPrivateKey, signingPublicKey, signingPublicKeyRaw, exchangePrivateKey, exchangePublicKey, exchangePublicKeyRaw, exchangePrivateKeyRaw, createdAt, framework, frameworkVersion) {
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
  static async generate() {
    try {
      const signingKeyPair = await crypto.subtle.generateKey(
        { name: "Ed25519" },
        true,
        // extractable
        ["sign", "verify"]
      );
      const exchangeKeyPair = await crypto.subtle.generateKey(
        { name: "X25519" },
        true,
        // extractable
        ["deriveBits"]
      );
      const signingPublicKeyRaw = new Uint8Array(
        await crypto.subtle.exportKey("raw", signingKeyPair.publicKey)
      );
      const exchangePublicKeyRaw = new Uint8Array(
        await crypto.subtle.exportKey("raw", exchangeKeyPair.publicKey)
      );
      const exchangePrivateKeyPkcs8 = await crypto.subtle.exportKey("pkcs8", exchangeKeyPair.privateKey);
      const exchangePrivateKeyRaw = unwrapX25519PrivateKeyPKCS8(new Uint8Array(exchangePrivateKeyPkcs8));
      const amid = await deriveAmid(signingPublicKeyRaw);
      return new _Identity(
        amid,
        signingKeyPair.privateKey,
        signingKeyPair.publicKey,
        signingPublicKeyRaw,
        exchangeKeyPair.privateKey,
        exchangeKeyPair.publicKey,
        exchangePublicKeyRaw,
        exchangePrivateKeyRaw,
        /* @__PURE__ */ new Date(),
        "agentmesh-js",
        "0.1.0"
      );
    } catch (error) {
      throw new chunkFNHOFD2H_cjs.CryptoError(
        `Failed to generate identity: ${error instanceof Error ? error.message : "Unknown error"}`,
        "KEY_GENERATION_FAILED"
      );
    }
  }
  /**
   * Load identity from storage.
   */
  static async load(storage, path) {
    const key = chunkFK3FEKXY_cjs.namespacedKey(chunkFK3FEKXY_cjs.StorageNamespace.IDENTITY, path);
    const data = await storage.get(key);
    if (!data) {
      throw new chunkFNHOFD2H_cjs.CryptoError(`Identity not found at ${path}`, "IDENTITY_NOT_FOUND");
    }
    try {
      const json = new TextDecoder().decode(data);
      const parsed = JSON.parse(json);
      return _Identity.fromData(parsed);
    } catch (error) {
      throw new chunkFNHOFD2H_cjs.CryptoError(
        `Failed to load identity: ${error instanceof Error ? error.message : "Unknown error"}`,
        "IDENTITY_LOAD_FAILED"
      );
    }
  }
  /**
   * Create identity from serialized data.
   */
  static async fromData(data) {
    try {
      const signingPrivateKeyB64 = stripKeyPrefix(data.signing_private_key, "ed25519:");
      const signingPublicKeyB64 = stripKeyPrefix(data.signing_public_key, "ed25519:");
      const signingPrivateKeyRaw = base64ToUint8Array(signingPrivateKeyB64);
      const signingPublicKeyRaw = base64ToUint8Array(signingPublicKeyB64);
      const signingKeyPair = await importEd25519KeyPair(signingPrivateKeyRaw, signingPublicKeyRaw);
      const exchangePrivateKeyB64 = stripKeyPrefix(data.exchange_private_key, "x25519:");
      const exchangePublicKeyB64 = stripKeyPrefix(data.exchange_public_key, "x25519:");
      const exchangePrivateKeyRaw = base64ToUint8Array(exchangePrivateKeyB64);
      const exchangePublicKeyRaw = base64ToUint8Array(exchangePublicKeyB64);
      const exchangeKeyPair = await importX25519KeyPair(exchangePrivateKeyRaw, exchangePublicKeyRaw);
      return new _Identity(
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
      throw new chunkFNHOFD2H_cjs.CryptoError(
        `Failed to import identity: ${error instanceof Error ? error.message : "Unknown error"}`,
        "IDENTITY_IMPORT_FAILED"
      );
    }
  }
  /**
   * Save identity to storage.
   */
  async save(storage, path) {
    const data = await this.toData();
    const json = JSON.stringify(data, null, 2);
    const bytes = new TextEncoder().encode(json);
    const key = chunkFK3FEKXY_cjs.namespacedKey(chunkFK3FEKXY_cjs.StorageNamespace.IDENTITY, path);
    await storage.set(key, bytes);
  }
  /**
   * Export identity as serializable data.
   */
  async toData() {
    const signingPrivateKeyRaw = await exportEd25519PrivateKey(this.signingPrivateKey);
    const exchangePrivateKeyRaw = await exportX25519PrivateKey(this.exchangePrivateKey);
    return {
      amid: this.amid,
      signing_private_key: "ed25519:" + arrayBufferToBase64(signingPrivateKeyRaw),
      signing_public_key: "ed25519:" + arrayBufferToBase64(this.signingPublicKeyRaw),
      exchange_private_key: "x25519:" + arrayBufferToBase64(exchangePrivateKeyRaw),
      exchange_public_key: "x25519:" + arrayBufferToBase64(this.exchangePublicKeyRaw),
      created_at: this.createdAt.toISOString(),
      framework: this.framework,
      framework_version: this.frameworkVersion
    };
  }
  /**
   * Get signing public key as base64 with type prefix.
   */
  get signingPublicKeyB64() {
    return "ed25519:" + arrayBufferToBase64(this.signingPublicKeyRaw);
  }
  /**
   * Get signing public key as base64 without prefix (for signature verification).
   */
  get signingPublicKeyB64Raw() {
    return arrayBufferToBase64(this.signingPublicKeyRaw);
  }
  /**
   * Get exchange public key as base64 with type prefix.
   */
  get exchangePublicKeyB64() {
    return "x25519:" + arrayBufferToBase64(this.exchangePublicKeyRaw);
  }
  /**
   * Get exchange public key as base64 without prefix.
   */
  get exchangePublicKeyB64Raw() {
    return arrayBufferToBase64(this.exchangePublicKeyRaw);
  }
  /**
   * Get raw signing public key bytes.
   */
  getSigningPublicKeyRaw() {
    return new Uint8Array(this.signingPublicKeyRaw);
  }
  /**
   * Get raw exchange public key bytes.
   */
  getExchangePublicKeyRaw() {
    return new Uint8Array(this.exchangePublicKeyRaw);
  }
  /**
   * Get the exchange private key as CryptoKey (for DH operations).
   */
  getExchangePrivateKey() {
    return this.exchangePrivateKey;
  }
  /**
   * Get the exchange private key as raw bytes (for X3DH).
   */
  getExchangePrivateKeyRaw() {
    return new Uint8Array(this.exchangePrivateKeyRaw);
  }
  /**
   * Sign a message with the signing key.
   */
  async sign(message) {
    try {
      const signature = await crypto.subtle.sign(
        { name: "Ed25519" },
        this.signingPrivateKey,
        toBufferSource(message)
      );
      return new Uint8Array(signature);
    } catch (error) {
      throw new chunkFNHOFD2H_cjs.CryptoError(
        `Failed to sign message: ${error instanceof Error ? error.message : "Unknown error"}`,
        "SIGN_FAILED"
      );
    }
  }
  /**
   * Sign a message and return base64-encoded signature.
   */
  async signB64(message) {
    const signature = await this.sign(message);
    return arrayBufferToBase64(signature);
  }
  /**
   * Sign the current timestamp for authentication.
   */
  async signTimestamp() {
    const timestamp = (/* @__PURE__ */ new Date()).toISOString();
    const message = new TextEncoder().encode(timestamp);
    const signature = await this.signB64(message);
    return [timestamp, signature];
  }
  /**
   * Verify a signature from another agent.
   */
  static async verifySignature(publicKeyB64, message, signatureB64) {
    try {
      let keyB64 = publicKeyB64;
      if (keyB64.startsWith("ed25519:")) {
        keyB64 = keyB64.slice(8);
      } else if (keyB64.startsWith("x25519:")) {
        keyB64 = keyB64.slice(7);
      }
      const publicKeyRaw = base64ToUint8Array(keyB64);
      const signature = base64ToUint8Array(signatureB64);
      const publicKey = await crypto.subtle.importKey(
        "raw",
        toArrayBuffer(publicKeyRaw),
        { name: "Ed25519" },
        false,
        ["verify"]
      );
      return await crypto.subtle.verify(
        { name: "Ed25519" },
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
  static async verifySignatureRaw(publicKeyRaw, message, signatureRaw) {
    try {
      const publicKey = await crypto.subtle.importKey(
        "raw",
        toArrayBuffer(publicKeyRaw),
        { name: "Ed25519" },
        false,
        ["verify"]
      );
      return await crypto.subtle.verify(
        { name: "Ed25519" },
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
  toPublicInfo() {
    return {
      amid: this.amid,
      signing_public_key: this.signingPublicKeyB64,
      exchange_public_key: this.exchangePublicKeyB64,
      framework: this.framework,
      framework_version: this.frameworkVersion
    };
  }
  /**
   * Derive a secret from the identity using HMAC-SHA256.
   * Used for deriving application-specific keys.
   */
  async deriveSecret(info) {
    const masterSecret = this.exchangePrivateKeyRaw;
    const hmacKey = await crypto.subtle.importKey(
      "raw",
      toArrayBuffer(masterSecret),
      { name: "HMAC", hash: "SHA-256" },
      false,
      ["sign"]
    );
    const infoBytes = new TextEncoder().encode(info);
    const derived = await crypto.subtle.sign("HMAC", hmacKey, toBufferSource(infoBytes));
    return new Uint8Array(derived);
  }
  /**
   * Rotate cryptographic keys.
   * Returns a new Identity with fresh keys.
   */
  async rotateKeys() {
    console.warn(`Key rotation: ${this.amid} will be replaced with new identity`);
    return _Identity.generate();
  }
};
async function importEd25519KeyPair(privateKeyRaw, publicKeyRaw) {
  const publicKey = await crypto.subtle.importKey(
    "raw",
    toArrayBuffer(publicKeyRaw),
    { name: "Ed25519" },
    true,
    ["verify"]
  );
  const pkcs8 = wrapEd25519PrivateKeyPKCS8(privateKeyRaw);
  const privateKey = await crypto.subtle.importKey(
    "pkcs8",
    toArrayBuffer(pkcs8),
    { name: "Ed25519" },
    true,
    ["sign"]
  );
  return { privateKey, publicKey };
}
async function importX25519KeyPair(privateKeyRaw, publicKeyRaw) {
  const publicKey = await crypto.subtle.importKey(
    "raw",
    toArrayBuffer(publicKeyRaw),
    { name: "X25519" },
    true,
    []
  );
  const pkcs8 = wrapX25519PrivateKeyPKCS8(privateKeyRaw);
  const privateKey = await crypto.subtle.importKey(
    "pkcs8",
    toArrayBuffer(pkcs8),
    { name: "X25519" },
    true,
    ["deriveBits"]
  );
  return { privateKey, publicKey };
}
async function exportEd25519PrivateKey(key) {
  const pkcs8 = await crypto.subtle.exportKey("pkcs8", key);
  return unwrapEd25519PrivateKeyPKCS8(new Uint8Array(pkcs8));
}
async function exportX25519PrivateKey(key) {
  const pkcs8 = await crypto.subtle.exportKey("pkcs8", key);
  return unwrapX25519PrivateKeyPKCS8(new Uint8Array(pkcs8));
}
var ED25519_PKCS8_PREFIX = new Uint8Array([
  48,
  46,
  // SEQUENCE, length 46
  2,
  1,
  0,
  // INTEGER version = 0
  48,
  5,
  // SEQUENCE algorithm
  6,
  3,
  43,
  101,
  112,
  // OID 1.3.101.112 (Ed25519)
  4,
  34,
  // OCTET STRING, length 34
  4,
  32
  // OCTET STRING (inner), length 32
]);
function wrapEd25519PrivateKeyPKCS8(rawKey) {
  const result = new Uint8Array(ED25519_PKCS8_PREFIX.length + rawKey.length);
  result.set(ED25519_PKCS8_PREFIX);
  result.set(rawKey, ED25519_PKCS8_PREFIX.length);
  return result;
}
function unwrapEd25519PrivateKeyPKCS8(pkcs8) {
  return pkcs8.slice(-32);
}
var X25519_PKCS8_PREFIX = new Uint8Array([
  48,
  46,
  // SEQUENCE, length 46
  2,
  1,
  0,
  // INTEGER version = 0
  48,
  5,
  // SEQUENCE algorithm
  6,
  3,
  43,
  101,
  110,
  // OID 1.3.101.110 (X25519)
  4,
  34,
  // OCTET STRING, length 34
  4,
  32
  // OCTET STRING (inner), length 32
]);
function wrapX25519PrivateKeyPKCS8(rawKey) {
  const result = new Uint8Array(X25519_PKCS8_PREFIX.length + rawKey.length);
  result.set(X25519_PKCS8_PREFIX);
  result.set(rawKey, X25519_PKCS8_PREFIX.length);
  return result;
}
function unwrapX25519PrivateKeyPKCS8(pkcs8) {
  return pkcs8.slice(-32);
}

exports.Identity = Identity;
//# sourceMappingURL=chunk-BPYP43TA.cjs.map
//# sourceMappingURL=chunk-BPYP43TA.cjs.map