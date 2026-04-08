/**
 * X3DH Prekey Bundle management.
 * Handles prekey generation, storage, and rotation.
 */

import { Identity } from '../identity';
import { Storage } from '../storage/interface';


/**
 * X25519 PKCS8 prefix for key wrapping.
 */
const X25519_PKCS8_PREFIX = new Uint8Array([
  0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x6e, 0x04, 0x22, 0x04, 0x20,
]);

/**
 * X25519 SPKI prefix for public key wrapping.
 */
const X25519_SPKI_PREFIX = new Uint8Array([
  0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x6e, 0x03, 0x21, 0x00,
]);

/**
 * One-time prekey pair.
 */
export interface OneTimePrekey {
  id: number;
  publicKey: Uint8Array;
  privateKey: Uint8Array;
}

/**
 * Signed prekey pair.
 */
export interface SignedPrekey {
  id: number;
  publicKey: Uint8Array;
  privateKey: Uint8Array;
  signature: Uint8Array;
  createdAt: Date;
}

/**
 * Public prekey bundle for X3DH - sent to registry.
 */
export interface PrekeyBundle {
  identityKey: Uint8Array; // X25519 public key
  signedPrekey: Uint8Array;
  signedPrekeySignature: Uint8Array;
  signedPrekeyId: number;
  oneTimePrekeys: Array<{ id: number; key: Uint8Array }>;
  uploadedAt?: Date;
}

/**
 * Serialized bundle for registry upload.
 */
export interface PrekeyBundleSerialized {
  identity_key: string;
  signed_prekey: string;
  signed_prekey_signature: string;
  signed_prekey_id: number;
  one_time_prekeys: Array<{ id: number; key: string }>;
  uploaded_at?: string;
}

/**
 * Local prekey state for persistence.
 */
export interface PrekeyState {
  signedPrekeyId: number;
  signedPrekeyPrivate: Uint8Array;
  signedPrekeyPublic: Uint8Array;
  signedPrekeyCreated: Date;
  oneTimePrekeys: Map<number, Uint8Array>; // id -> private key
  oneTimePrekeyPublicKeys: Map<number, Uint8Array>; // id -> public key
  nextPrekeyId: number;
  consumedPrekeyIds: number[];
  // Grace period for old signed prekey
  oldSignedPrekeyPrivate?: Uint8Array;
  oldSignedPrekeyId?: number;
  oldSignedPrekeyExpires?: Date;
}

/**
 * Prekey configuration constants.
 */
export const PREKEY_CONFIG = {
  ONE_TIME_PREKEY_COUNT: 100,
  PREKEY_LOW_THRESHOLD: 20,
  SIGNED_PREKEY_ROTATION_DAYS: 7,
  SIGNED_PREKEY_GRACE_PERIOD_HOURS: 24,
} as const;

/**
 * Generate an X25519 keypair using Web Crypto.
 */
export async function generateX25519Keypair(): Promise<{
  publicKey: Uint8Array;
  privateKey: Uint8Array;
}> {
  const keyPair = await crypto.subtle.generateKey('X25519' as never, true, [
    'deriveBits',
  ]) as CryptoKeyPair;

  // Export keys
  const publicKeySpki = await crypto.subtle.exportKey('spki', keyPair.publicKey);
  const privateKeyPkcs8 = await crypto.subtle.exportKey('pkcs8', keyPair.privateKey);

  // Extract raw keys from SPKI/PKCS8 formats
  const publicKey = new Uint8Array(publicKeySpki).slice(X25519_SPKI_PREFIX.length);
  const privateKey = new Uint8Array(privateKeyPkcs8).slice(X25519_PKCS8_PREFIX.length);

  return { publicKey, privateKey };
}

/**
 * Import X25519 private key for DH operations.
 */
export async function importX25519PrivateKey(privateKey: Uint8Array): Promise<CryptoKey> {
  // Wrap raw key in PKCS8 format
  const pkcs8 = new Uint8Array(X25519_PKCS8_PREFIX.length + privateKey.length);
  pkcs8.set(X25519_PKCS8_PREFIX, 0);
  pkcs8.set(privateKey, X25519_PKCS8_PREFIX.length);

  return crypto.subtle.importKey(
    'pkcs8',
    pkcs8,
    { name: 'X25519' } as never,
    false,
    ['deriveBits']
  );
}

/**
 * Import X25519 public key for DH operations.
 */
export async function importX25519PublicKey(publicKey: Uint8Array): Promise<CryptoKey> {
  // Wrap raw key in SPKI format
  const spki = new Uint8Array(X25519_SPKI_PREFIX.length + publicKey.length);
  spki.set(X25519_SPKI_PREFIX, 0);
  spki.set(publicKey, X25519_SPKI_PREFIX.length);

  return crypto.subtle.importKey(
    'spki',
    spki,
    { name: 'X25519' } as never,
    false,
    []
  );
}

/**
 * Perform X25519 Diffie-Hellman.
 */
export async function x25519DH(
  privateKey: Uint8Array,
  publicKey: Uint8Array
): Promise<Uint8Array> {
  if (privateKey.length !== 32 || publicKey.length !== 32) {
    throw new Error(`Invalid X25519 key length: private=${privateKey.length} public=${publicKey.length}`);
  }

  const privKey = await importX25519PrivateKey(privateKey);
  const pubKey = await importX25519PublicKey(publicKey);

  const sharedBits = await crypto.subtle.deriveBits(
    { name: 'X25519', public: pubKey } as never,
    privKey,
    256
  );

  const result = new Uint8Array(sharedBits);

  // Check for all-zero DH result (low-order point attack)
  if (result.every(b => b === 0)) {
    throw new Error('X25519 DH produced all-zero output — possible low-order point attack');
  }

  return result;
}

/**
 * Generate a signed prekey.
 */
export async function generateSignedPrekey(
  identity: Identity,
  id: number
): Promise<SignedPrekey> {
  const { publicKey, privateKey } = await generateX25519Keypair();

  // Sign the public key with identity's signing key
  const signature = await identity.sign(publicKey);

  return {
    id,
    publicKey,
    privateKey,
    signature,
    createdAt: new Date(),
  };
}

/**
 * Generate one-time prekeys.
 */
export async function generateOneTimePrekeys(
  startId: number,
  count: number
): Promise<OneTimePrekey[]> {
  const prekeys: OneTimePrekey[] = [];

  for (let i = 0; i < count; i++) {
    const { publicKey, privateKey } = await generateX25519Keypair();
    prekeys.push({
      id: startId + i,
      publicKey,
      privateKey,
    });
  }

  return prekeys;
}

/**
 * Serialize prekey bundle for registry upload.
 */
export function serializePrekeyBundle(bundle: PrekeyBundle): PrekeyBundleSerialized {
  const toBase64 = (bytes: Uint8Array) => {
    const binary = String.fromCharCode(...bytes);
    return btoa(binary);
  };

  return {
    identity_key: toBase64(bundle.identityKey),
    signed_prekey: toBase64(bundle.signedPrekey),
    signed_prekey_signature: toBase64(bundle.signedPrekeySignature),
    signed_prekey_id: bundle.signedPrekeyId,
    one_time_prekeys: bundle.oneTimePrekeys.map((pk) => ({
      id: pk.id,
      key: toBase64(pk.key),
    })),
    uploaded_at: bundle.uploadedAt?.toISOString(),
  };
}

/**
 * Deserialize prekey bundle from registry response.
 */
export function deserializePrekeyBundle(data: PrekeyBundleSerialized): PrekeyBundle {
  const stripKeyPrefix = (s: string): string => {
    if (s.startsWith('ed25519:')) return s.slice(8);
    if (s.startsWith('x25519:')) return s.slice(7);
    return s;
  };
  const fromBase64 = (b64: string): Uint8Array => {
    const binary = atob(stripKeyPrefix(b64));
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) {
      bytes[i] = binary.charCodeAt(i);
    }
    return bytes;
  };

  return {
    identityKey: fromBase64(data.identity_key),
    signedPrekey: fromBase64(data.signed_prekey),
    signedPrekeySignature: fromBase64(data.signed_prekey_signature),
    signedPrekeyId: data.signed_prekey_id,
    oneTimePrekeys: (data.one_time_prekeys ?? []).map((pk) => ({
      id: pk.id,
      key: fromBase64(pk.key),
    })),
    uploadedAt: data.uploaded_at ? new Date(data.uploaded_at) : undefined,
  };
}

/**
 * Prekey Manager - handles prekey lifecycle.
 */
export class PrekeyManager {
  private readonly identity: Identity;
  private readonly storage: Storage;
  private state: PrekeyState | null = null;
  private readonly storagePath = 'prekeys/state.json';

  constructor(identity: Identity, storage: Storage) {
    this.identity = identity;
    this.storage = storage;
  }

  /**
   * Load or initialize prekeys.
   */
  async loadOrInitialize(): Promise<PrekeyBundle> {
    const data = await this.storage.get(this.storagePath);

    if (data) {
      try {
        this.state = this.deserializeState(data);

        // Check if signed prekey needs rotation
        const age = Date.now() - this.state.signedPrekeyCreated.getTime();
        const maxAge = PREKEY_CONFIG.SIGNED_PREKEY_ROTATION_DAYS * 24 * 60 * 60 * 1000;

        if (age > maxAge) {
          await this.rotateSignedPrekey();
        }

        // Check if replenishment needed
        if (this.needsReplenishment()) {
          await this.replenishPrekeys();
        }

        return await this.buildBundle();
      } catch {
        // Corrupted state, regenerate
      }
    }

    return this.generateInitialPrekeys();
  }

  /**
   * Generate initial prekey bundle.
   */
  private async generateInitialPrekeys(): Promise<PrekeyBundle> {
    const signedPrekey = await generateSignedPrekey(this.identity, 1);
    const oneTimePrekeys = await generateOneTimePrekeys(1, PREKEY_CONFIG.ONE_TIME_PREKEY_COUNT);

    const prekeyMap = new Map<number, Uint8Array>();
    const prekeyPubMap = new Map<number, Uint8Array>();
    for (const pk of oneTimePrekeys) {
      prekeyMap.set(pk.id, pk.privateKey);
      prekeyPubMap.set(pk.id, pk.publicKey);
    }

    this.state = {
      signedPrekeyId: 1,
      signedPrekeyPrivate: signedPrekey.privateKey,
      signedPrekeyPublic: signedPrekey.publicKey,
      signedPrekeyCreated: new Date(),
      oneTimePrekeys: prekeyMap,
      oneTimePrekeyPublicKeys: prekeyPubMap,
      nextPrekeyId: PREKEY_CONFIG.ONE_TIME_PREKEY_COUNT + 1,
      consumedPrekeyIds: [],
    };

    await this.saveState();

    return {
      identityKey: this.identity.getExchangePublicKeyRaw(),
      signedPrekey: signedPrekey.publicKey,
      signedPrekeySignature: signedPrekey.signature,
      signedPrekeyId: 1,
      oneTimePrekeys: oneTimePrekeys.map((pk) => ({ id: pk.id, key: pk.publicKey })),
      uploadedAt: new Date(),
    };
  }

  /**
   * Rotate signed prekey (every 7 days).
   */
  async rotateSignedPrekey(): Promise<PrekeyBundle> {
    if (!this.state) {
      throw new Error('Prekey state not initialized');
    }

    // Save old prekey for grace period
    this.state.oldSignedPrekeyPrivate = this.state.signedPrekeyPrivate;
    this.state.oldSignedPrekeyId = this.state.signedPrekeyId;
    this.state.oldSignedPrekeyExpires = new Date(
      Date.now() + PREKEY_CONFIG.SIGNED_PREKEY_GRACE_PERIOD_HOURS * 60 * 60 * 1000
    );

    // Generate new signed prekey
    const newId = this.state.signedPrekeyId + 1;
    const signedPrekey = await generateSignedPrekey(this.identity, newId);

    this.state.signedPrekeyId = newId;
    this.state.signedPrekeyPrivate = signedPrekey.privateKey;
    this.state.signedPrekeyPublic = signedPrekey.publicKey;
    this.state.signedPrekeyCreated = new Date();

    await this.saveState();

    return await this.buildBundle();
  }

  /**
   * Replenish one-time prekeys when running low.
   */
  async replenishPrekeys(): Promise<Array<{ id: number; key: Uint8Array }>> {
    if (!this.state) {
      throw new Error('Prekey state not initialized');
    }

    const currentCount = this.state.oneTimePrekeys.size;
    const countToGenerate = PREKEY_CONFIG.ONE_TIME_PREKEY_COUNT - currentCount;

    if (countToGenerate <= 0) {
      return [];
    }

    const newPrekeys = await generateOneTimePrekeys(this.state.nextPrekeyId, countToGenerate);

    for (const pk of newPrekeys) {
      this.state.oneTimePrekeys.set(pk.id, pk.privateKey);
      this.state.oneTimePrekeyPublicKeys.set(pk.id, pk.publicKey);
    }
    this.state.nextPrekeyId += countToGenerate;

    await this.saveState();

    return newPrekeys.map((pk) => ({ id: pk.id, key: pk.publicKey }));
  }

  /**
   * Get signed prekey private key by ID.
   */
  getSignedPrekeyPrivate(id: number): Uint8Array | null {
    if (!this.state) return null;

    if (id === this.state.signedPrekeyId) {
      return this.state.signedPrekeyPrivate;
    }

    // Check old prekey during grace period
    if (
      this.state.oldSignedPrekeyId === id &&
      this.state.oldSignedPrekeyExpires &&
      new Date() < this.state.oldSignedPrekeyExpires
    ) {
      return this.state.oldSignedPrekeyPrivate ?? null;
    }

    return null;
  }

  /**
   * Get signed prekey public key by ID.
   */
  getSignedPrekeyPublic(id: number): Uint8Array | null {
    if (!this.state) return null;
    if (id === this.state.signedPrekeyId) {
      return this.state.signedPrekeyPublic;
    }
    return null;
  }

  /**
   * Get one-time prekey private key by ID.
   */
  getOneTimePrekeyPrivate(id: number): Uint8Array | null {
    if (!this.state) return null;
    return this.state.oneTimePrekeys.get(id) ?? null;
  }

  /**
   * Consume a one-time prekey (mark as used).
   */
  async consumePrekey(id: number): Promise<void> {
    if (!this.state) return;

    this.state.oneTimePrekeys.delete(id);
    this.state.oneTimePrekeyPublicKeys.delete(id);
    this.state.consumedPrekeyIds.push(id);

    // Keep only last 1000 consumed IDs
    if (this.state.consumedPrekeyIds.length > 1000) {
      this.state.consumedPrekeyIds = this.state.consumedPrekeyIds.slice(-1000);
    }

    await this.saveState();
  }

  /**
   * Check if prekey was already consumed.
   */
  isPrekeyConsumed(id: number): boolean {
    return this.state?.consumedPrekeyIds.includes(id) ?? false;
  }

  /**
   * Check if replenishment is needed.
   */
  needsReplenishment(): boolean {
    if (!this.state) return true;
    return this.state.oneTimePrekeys.size < PREKEY_CONFIG.PREKEY_LOW_THRESHOLD;
  }

  /**
   * Get remaining prekey count.
   */
  remainingPrekeyCount(): number {
    return this.state?.oneTimePrekeys.size ?? 0;
  }

  /**
   * Build prekey bundle from current state.
   * Re-signs the signed prekey and includes stored public keys.
   */
  private async buildBundle(): Promise<PrekeyBundle> {
    if (!this.state) {
      throw new Error('Prekey state not initialized');
    }

    // Re-sign the signed prekey with the identity's signing key
    const signedPrekeySignature = await this.identity.sign(this.state.signedPrekeyPublic);

    // Collect one-time prekey public keys from stored state
    const oneTimePrekeys: Array<{ id: number; key: Uint8Array }> = [];
    for (const [id, publicKey] of this.state.oneTimePrekeyPublicKeys) {
      oneTimePrekeys.push({ id, key: publicKey });
    }

    return {
      identityKey: this.identity.getExchangePublicKeyRaw(),
      signedPrekey: this.state.signedPrekeyPublic,
      signedPrekeySignature,
      signedPrekeyId: this.state.signedPrekeyId,
      oneTimePrekeys,
      uploadedAt: new Date(),
    };
  }

  /**
   * Serialize state for storage.
   */
  private serializeState(): Uint8Array {
    if (!this.state) {
      throw new Error('No state to serialize');
    }

    const toBase64 = (bytes: Uint8Array) => {
      const binary = String.fromCharCode(...bytes);
      return btoa(binary);
    };

    const oneTimePrekeys: Record<string, string> = {};
    for (const [id, privateKey] of this.state.oneTimePrekeys) {
      oneTimePrekeys[String(id)] = toBase64(privateKey);
    }

    const oneTimePrekeyPublicKeys: Record<string, string> = {};
    for (const [id, publicKey] of this.state.oneTimePrekeyPublicKeys) {
      oneTimePrekeyPublicKeys[String(id)] = toBase64(publicKey);
    }

    const data = {
      signedPrekeyId: this.state.signedPrekeyId,
      signedPrekeyPrivate: toBase64(this.state.signedPrekeyPrivate),
      signedPrekeyPublic: toBase64(this.state.signedPrekeyPublic),
      signedPrekeyCreated: this.state.signedPrekeyCreated.toISOString(),
      oneTimePrekeys,
      oneTimePrekeyPublicKeys,
      nextPrekeyId: this.state.nextPrekeyId,
      consumedPrekeyIds: this.state.consumedPrekeyIds,
      oldSignedPrekeyPrivate: this.state.oldSignedPrekeyPrivate
        ? toBase64(this.state.oldSignedPrekeyPrivate)
        : undefined,
      oldSignedPrekeyId: this.state.oldSignedPrekeyId,
      oldSignedPrekeyExpires: this.state.oldSignedPrekeyExpires?.toISOString(),
    };

    return new TextEncoder().encode(JSON.stringify(data, null, 2));
  }

  /**
   * Deserialize state from storage.
   */
  private deserializeState(data: Uint8Array): PrekeyState {
    const fromBase64 = (b64: string): Uint8Array => {
      const binary = atob(b64);
      const bytes = new Uint8Array(binary.length);
      for (let i = 0; i < binary.length; i++) {
        bytes[i] = binary.charCodeAt(i);
      }
      return bytes;
    };

    const json = JSON.parse(new TextDecoder().decode(data));

    const oneTimePrekeys = new Map<number, Uint8Array>();
    for (const [id, b64] of Object.entries(json.oneTimePrekeys as Record<string, string>)) {
      oneTimePrekeys.set(parseInt(id, 10), fromBase64(b64));
    }

    const oneTimePrekeyPublicKeys = new Map<number, Uint8Array>();
    if (json.oneTimePrekeyPublicKeys) {
      for (const [id, b64] of Object.entries(json.oneTimePrekeyPublicKeys as Record<string, string>)) {
        oneTimePrekeyPublicKeys.set(parseInt(id, 10), fromBase64(b64));
      }
    }

    return {
      signedPrekeyId: json.signedPrekeyId,
      signedPrekeyPrivate: fromBase64(json.signedPrekeyPrivate),
      signedPrekeyPublic: fromBase64(json.signedPrekeyPublic),
      signedPrekeyCreated: new Date(json.signedPrekeyCreated),
      oneTimePrekeys,
      oneTimePrekeyPublicKeys,
      nextPrekeyId: json.nextPrekeyId,
      consumedPrekeyIds: json.consumedPrekeyIds ?? [],
      oldSignedPrekeyPrivate: json.oldSignedPrekeyPrivate
        ? fromBase64(json.oldSignedPrekeyPrivate)
        : undefined,
      oldSignedPrekeyId: json.oldSignedPrekeyId,
      oldSignedPrekeyExpires: json.oldSignedPrekeyExpires
        ? new Date(json.oldSignedPrekeyExpires)
        : undefined,
    };
  }

  /**
   * Save state to storage.
   */
  private async saveState(): Promise<void> {
    await this.storage.set(this.storagePath, this.serializeState());
  }
}
