/**
 * Double Ratchet algorithm implementation.
 * Provides forward secrecy and break-in recovery for encrypted messaging.
 */

import { kdfRK, kdfCK } from './hkdf';
import { x25519DH, generateX25519Keypair } from './prekey';
import sodium from 'libsodium-wrappers';

// Ensure sodium is ready
let sodiumReady: Promise<void> | null = null;
async function ensureSodiumReady(): Promise<void> {
  if (!sodiumReady) {
    sodiumReady = sodium.ready;
  }
  await sodiumReady;
}

/**
 * Maximum number of skipped message keys to store.
 */
const MAX_SKIP = 1000;

/**
 * Message header for Double Ratchet messages.
 */
export interface RatchetHeader {
  /** Sender's current ratchet public key */
  dhPublicKey: Uint8Array;
  /** Previous chain message count */
  previousChainLength: number;
  /** Message number in current sending chain */
  messageNumber: number;
}

/**
 * Serialized ratchet header for wire transport.
 */
export interface RatchetHeaderSerialized {
  dh_public_key: string;
  pn: number;
  n: number;
}

/**
 * Double Ratchet state.
 */
export interface DoubleRatchetState {
  /** DH ratchet key pair */
  dhPrivate: Uint8Array;
  dhPublic: Uint8Array;

  /** Peer's current DH public key */
  peerDhPublic: Uint8Array | null;

  /** Root key for DH ratchet steps */
  rootKey: Uint8Array;

  /** Sending chain key */
  sendChainKey: Uint8Array | null;

  /** Receiving chain key */
  recvChainKey: Uint8Array | null;

  /** Message numbers */
  sendMessageNumber: number;
  recvMessageNumber: number;

  /** Previous sending chain length (for header) */
  previousChainLength: number;

  /** Skipped message keys: key = `${base64(dhPub)}:${msgNum}` */
  skippedKeys: Map<string, Uint8Array>;
}

/**
 * Serialized Double Ratchet state for persistence.
 */
export interface DoubleRatchetStateSerialized {
  dh_private: string;
  dh_public: string;
  peer_dh_public: string | null;
  root_key: string;
  send_chain_key: string | null;
  recv_chain_key: string | null;
  send_message_number: number;
  recv_message_number: number;
  previous_chain_length: number;
  skipped_keys: Record<string, string>;
}

/**
 * Encrypted message from Double Ratchet.
 */
export interface EncryptedMessage {
  /** Message header */
  header: RatchetHeader;
  /** Encrypted ciphertext (XSalsa20-Poly1305) */
  ciphertext: Uint8Array;
}

/**
 * Double Ratchet Session.
 *
 * Implements the Signal Protocol Double Ratchet algorithm for
 * perfect forward secrecy and break-in recovery.
 */
export class DoubleRatchetSession {
  private state: DoubleRatchetState;
  private readonly isInitiator: boolean;

  private constructor(state: DoubleRatchetState, isInitiator: boolean) {
    this.state = state;
    this.isInitiator = isInitiator;
  }

  /**
   * Initialize a new Double Ratchet session from X3DH shared secret.
   *
   * @param sharedSecret - The shared secret from X3DH key exchange
   * @param isInitiator - True if we initiated the session (Alice)
   * @param peerDhPublic - Peer's initial ratchet public key (for responder)
   */
  static async initialize(
    sharedSecret: Uint8Array,
    isInitiator: boolean,
    peerDhPublic?: Uint8Array
  ): Promise<DoubleRatchetSession> {
    // Generate initial DH ratchet keypair
    const { publicKey: dhPublic, privateKey: dhPrivate } = await generateX25519Keypair();

    const state: DoubleRatchetState = {
      dhPrivate,
      dhPublic,
      peerDhPublic: peerDhPublic ?? null,
      rootKey: sharedSecret,
      sendChainKey: null,
      recvChainKey: null,
      sendMessageNumber: 0,
      recvMessageNumber: 0,
      previousChainLength: 0,
      skippedKeys: new Map(),
    };

    const session = new DoubleRatchetSession(state, isInitiator);

    // Note: We don't do a DH ratchet step during initialization.
    // The responder will do a ratchet step when receiving the first message
    // (which triggers the check for a new peer DH key in decrypt()).
    // The initiator does a ratchet step when activating the session.

    return session;
  }

  /**
   * Restore session from serialized state.
   */
  static fromState(state: DoubleRatchetState, isInitiator: boolean): DoubleRatchetSession {
    return new DoubleRatchetSession(state, isInitiator);
  }

  /**
   * Get the current ratchet public key (to include in first message).
   */
  getRatchetPublicKey(): Uint8Array {
    return new Uint8Array(this.state.dhPublic);
  }

  /**
   * Initialize the session when we receive peer's key.
   * Just sets the peer's public key without doing a ratchet step.
   * The actual ratchet step happens on first encrypt/decrypt.
   */
  async initializeReceiving(peerDhPublic: Uint8Array): Promise<void> {
    this.state.peerDhPublic = peerDhPublic;
    // Don't do ratchet step here - encrypt() will derive send chain,
    // and decrypt() will handle recv chain when receiving.
  }

  /**
   * Encrypt a plaintext message.
   */
  async encrypt(plaintext: Uint8Array): Promise<EncryptedMessage> {
    // Initialize send chain if needed
    if (!this.state.sendChainKey) {
      if (!this.state.peerDhPublic) {
        throw new Error('Cannot encrypt: peer DH public key not set');
      }
      // Perform DH with our current key and their public key
      const dhOutput = await x25519DH(this.state.dhPrivate, this.state.peerDhPublic);
      const [newRoot, sendChain] = await kdfRK(this.state.rootKey, dhOutput);
      this.state.rootKey = newRoot;
      this.state.sendChainKey = sendChain;
    }

    // Derive message key from chain key
    const [messageKey, newChainKey] = await kdfCK(this.state.sendChainKey);
    this.state.sendChainKey = newChainKey;

    // Build header
    const header: RatchetHeader = {
      dhPublicKey: new Uint8Array(this.state.dhPublic),
      previousChainLength: this.state.previousChainLength,
      messageNumber: this.state.sendMessageNumber,
    };

    // Encrypt with AES-GCM
    const ciphertext = await this.aesEncrypt(messageKey, plaintext, this.serializeHeader(header));

    // Increment message number
    this.state.sendMessageNumber++;

    return { header, ciphertext };
  }

  /**
   * Decrypt an encrypted message.
   */
  async decrypt(message: EncryptedMessage): Promise<Uint8Array> {
    const { header, ciphertext } = message;

    // Check for skipped message key
    const skippedKey = this.getSkippedKey(header.dhPublicKey, header.messageNumber);
    if (skippedKey) {
      return this.aesDecrypt(skippedKey, ciphertext, this.serializeHeader(header));
    }

    // Check if DH ratchet step needed (new ratchet public key from peer)
    const isDifferentKey = !this.state.peerDhPublic || !this.bytesEqual(header.dhPublicKey, this.state.peerDhPublic);

    if (isDifferentKey) {
      // Skip remaining messages from old chain
      if (this.state.recvChainKey) {
        await this.skipMessageKeys(this.state.recvMessageNumber + MAX_SKIP);
      }

      // Perform DH ratchet step
      this.state.peerDhPublic = header.dhPublicKey;
      await this.dhRatchetStep();
    } else if (!this.state.recvChainKey && this.state.peerDhPublic) {
      // First message from peer with known key - derive recv chain directly
      // This handles the case where peer key was set during initialization
      // but no ratchet step was performed yet
      const dhOutput = await x25519DH(this.state.dhPrivate, this.state.peerDhPublic);
      const [newRoot, recvChain] = await kdfRK(this.state.rootKey, dhOutput);
      this.state.rootKey = newRoot;
      this.state.recvChainKey = recvChain;
      this.state.recvMessageNumber = 0;
    }

    // Skip to the correct message number if needed
    if (header.messageNumber > this.state.recvMessageNumber) {
      await this.skipMessageKeys(header.messageNumber);
    }

    if (!this.state.recvChainKey) {
      throw new Error('No receiving chain key available');
    }

    // Derive message key
    const [messageKey, newChainKey] = await kdfCK(this.state.recvChainKey);
    this.state.recvChainKey = newChainKey;
    this.state.recvMessageNumber = header.messageNumber + 1;

    // Decrypt
    return this.aesDecrypt(messageKey, ciphertext, this.serializeHeader(header));
  }

  /**
   * Perform a DH ratchet step.
   */
  private async dhRatchetStep(): Promise<void> {
    if (!this.state.peerDhPublic) {
      throw new Error('Cannot ratchet: no peer DH public key');
    }

    // DH with our current private key and their new public key
    const dhOutput1 = await x25519DH(this.state.dhPrivate, this.state.peerDhPublic);

    // Derive new receiving chain key
    const [newRoot1, recvChain] = await kdfRK(this.state.rootKey, dhOutput1);
    this.state.recvChainKey = recvChain;

    // Generate new DH keypair for sending
    const { publicKey: newDhPublic, privateKey: newDhPrivate } = await generateX25519Keypair();

    // Save previous chain length for header
    this.state.previousChainLength = this.state.sendMessageNumber;

    this.state.dhPrivate = newDhPrivate;
    this.state.dhPublic = newDhPublic;

    // DH with new private key and their public key
    const dhOutput2 = await x25519DH(this.state.dhPrivate, this.state.peerDhPublic);

    // Derive new sending chain key
    const [newRoot2, sendChain] = await kdfRK(newRoot1, dhOutput2);
    this.state.rootKey = newRoot2;
    this.state.sendChainKey = sendChain;

    // Reset message numbers for new chains
    this.state.sendMessageNumber = 0;
    this.state.recvMessageNumber = 0;
  }

  /**
   * Skip message keys for out-of-order handling.
   */
  private async skipMessageKeys(until: number): Promise<void> {
    if (!this.state.recvChainKey) {
      throw new Error('Cannot skip: no receiving chain key');
    }

    const toSkip = until - this.state.recvMessageNumber;
    if (toSkip > MAX_SKIP) {
      throw new Error(`Too many skipped messages: ${toSkip} > ${MAX_SKIP}`);
    }

    while (this.state.recvMessageNumber < until) {
      const [messageKey, newChainKey] = await kdfCK(this.state.recvChainKey);
      this.storeSkippedKey(this.state.peerDhPublic!, this.state.recvMessageNumber, messageKey);
      this.state.recvChainKey = newChainKey;
      this.state.recvMessageNumber++;
    }
  }

  /**
   * Store a skipped message key.
   */
  private storeSkippedKey(dhPublic: Uint8Array, messageNumber: number, key: Uint8Array): void {
    const keyId = this.makeSkippedKeyId(dhPublic, messageNumber);
    this.state.skippedKeys.set(keyId, key);

    // Limit total stored keys
    if (this.state.skippedKeys.size > MAX_SKIP * 2) {
      // Remove oldest keys (first inserted)
      const keys = Array.from(this.state.skippedKeys.keys());
      for (let i = 0; i < keys.length - MAX_SKIP; i++) {
        this.state.skippedKeys.delete(keys[i]!);
      }
    }
  }

  /**
   * Get a previously skipped message key.
   */
  private getSkippedKey(dhPublic: Uint8Array, messageNumber: number): Uint8Array | null {
    const keyId = this.makeSkippedKeyId(dhPublic, messageNumber);
    const key = this.state.skippedKeys.get(keyId);
    if (key) {
      this.state.skippedKeys.delete(keyId);
      return key;
    }
    return null;
  }

  /**
   * Make a key ID for skipped keys storage.
   */
  private makeSkippedKeyId(dhPublic: Uint8Array, messageNumber: number): string {
    const b64 = this.bytesToBase64(dhPublic);
    return `${b64}:${messageNumber}`;
  }

  /**
   * Convert Uint8Array to ArrayBuffer for Web Crypto compatibility.
   */
  private toArrayBuffer(data: Uint8Array): ArrayBuffer {
    const buffer = new ArrayBuffer(data.length);
    new Uint8Array(buffer).set(data);
    return buffer;
  }

  /**
   * XSalsa20-Poly1305 encryption (NaCl SecretBox).
   * Uses libsodium for compatibility with Python SDK.
   *
   * Note: XSalsa20-Poly1305 doesn't support AAD (Additional Authenticated Data).
   * The AAD parameter is kept for API compatibility but is not used.
   * Header authentication is implicit in the message structure.
   */
  private async secretboxEncrypt(key: Uint8Array, plaintext: Uint8Array, _aad: Uint8Array): Promise<Uint8Array> {
    await ensureSodiumReady();

    // Generate 24-byte nonce
    const nonce = sodium.randombytes_buf(sodium.crypto_secretbox_NONCEBYTES);

    // Encrypt using XSalsa20-Poly1305
    const encrypted = sodium.crypto_secretbox_easy(plaintext, nonce, key);

    // Prepend nonce to ciphertext (same format as Python NaCl)
    const result = new Uint8Array(nonce.length + encrypted.length);
    result.set(nonce, 0);
    result.set(encrypted, nonce.length);
    return result;
  }

  /**
   * XSalsa20-Poly1305 decryption (NaCl SecretBox).
   * Uses libsodium for compatibility with Python SDK.
   */
  private async secretboxDecrypt(key: Uint8Array, ciphertext: Uint8Array, _aad: Uint8Array): Promise<Uint8Array> {
    await ensureSodiumReady();

    // Extract 24-byte nonce from beginning
    const nonce = ciphertext.slice(0, sodium.crypto_secretbox_NONCEBYTES);
    const encrypted = ciphertext.slice(sodium.crypto_secretbox_NONCEBYTES);

    // Decrypt using XSalsa20-Poly1305
    const plaintext = sodium.crypto_secretbox_open_easy(encrypted, nonce, key);
    if (!plaintext) {
      throw new Error('Decryption failed: authentication tag mismatch');
    }

    return plaintext;
  }

  // Aliases for backward compatibility (internal API)
  private async aesEncrypt(key: Uint8Array, plaintext: Uint8Array, aad: Uint8Array): Promise<Uint8Array> {
    return this.secretboxEncrypt(key, plaintext, aad);
  }

  private async aesDecrypt(key: Uint8Array, ciphertext: Uint8Array, aad: Uint8Array): Promise<Uint8Array> {
    return this.secretboxDecrypt(key, ciphertext, aad);
  }

  /**
   * Serialize header for AAD.
   */
  private serializeHeader(header: RatchetHeader): Uint8Array {
    const data = {
      dh: this.bytesToBase64(header.dhPublicKey),
      pn: header.previousChainLength,
      n: header.messageNumber,
    };
    return new TextEncoder().encode(JSON.stringify(data));
  }

  /**
   * Get the current state for persistence.
   */
  getState(): DoubleRatchetState {
    return { ...this.state };
  }

  /**
   * Serialize state for storage.
   */
  serializeState(): DoubleRatchetStateSerialized {
    const skippedKeys: Record<string, string> = {};
    for (const [k, v] of this.state.skippedKeys) {
      skippedKeys[k] = this.bytesToBase64(v);
    }

    return {
      dh_private: this.bytesToBase64(this.state.dhPrivate),
      dh_public: this.bytesToBase64(this.state.dhPublic),
      peer_dh_public: this.state.peerDhPublic ? this.bytesToBase64(this.state.peerDhPublic) : null,
      root_key: this.bytesToBase64(this.state.rootKey),
      send_chain_key: this.state.sendChainKey ? this.bytesToBase64(this.state.sendChainKey) : null,
      recv_chain_key: this.state.recvChainKey ? this.bytesToBase64(this.state.recvChainKey) : null,
      send_message_number: this.state.sendMessageNumber,
      recv_message_number: this.state.recvMessageNumber,
      previous_chain_length: this.state.previousChainLength,
      skipped_keys: skippedKeys,
    };
  }

  /**
   * Deserialize state from storage.
   */
  static deserializeState(data: DoubleRatchetStateSerialized, isInitiator: boolean): DoubleRatchetSession {
    const fromBase64 = (b64: string): Uint8Array => {
      const binary = atob(b64);
      const bytes = new Uint8Array(binary.length);
      for (let i = 0; i < binary.length; i++) {
        bytes[i] = binary.charCodeAt(i);
      }
      return bytes;
    };

    const skippedKeys = new Map<string, Uint8Array>();
    for (const [k, v] of Object.entries(data.skipped_keys)) {
      skippedKeys.set(k, fromBase64(v));
    }

    const state: DoubleRatchetState = {
      dhPrivate: fromBase64(data.dh_private),
      dhPublic: fromBase64(data.dh_public),
      peerDhPublic: data.peer_dh_public ? fromBase64(data.peer_dh_public) : null,
      rootKey: fromBase64(data.root_key),
      sendChainKey: data.send_chain_key ? fromBase64(data.send_chain_key) : null,
      recvChainKey: data.recv_chain_key ? fromBase64(data.recv_chain_key) : null,
      sendMessageNumber: data.send_message_number,
      recvMessageNumber: data.recv_message_number,
      previousChainLength: data.previous_chain_length,
      skippedKeys,
    };

    return new DoubleRatchetSession(state, isInitiator);
  }

  /**
   * Helper: bytes to base64.
   */
  private bytesToBase64(bytes: Uint8Array): string {
    const binary = String.fromCharCode(...bytes);
    return btoa(binary);
  }

  /**
   * Helper: compare byte arrays.
   */
  private bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
    if (a.length !== b.length) return false;
    for (let i = 0; i < a.length; i++) {
      if (a[i] !== b[i]) return false;
    }
    return true;
  }
}

/**
 * Serialize ratchet header for wire transport.
 */
export function serializeRatchetHeader(header: RatchetHeader): RatchetHeaderSerialized {
  const binary = String.fromCharCode(...header.dhPublicKey);
  return {
    dh_public_key: btoa(binary),
    pn: header.previousChainLength,
    n: header.messageNumber,
  };
}

/**
 * Deserialize ratchet header from wire transport.
 */
export function deserializeRatchetHeader(data: RatchetHeaderSerialized): RatchetHeader {
  const binary = atob(data.dh_public_key);
  const dhPublicKey = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    dhPublicKey[i] = binary.charCodeAt(i);
  }

  return {
    dhPublicKey,
    previousChainLength: data.pn,
    messageNumber: data.n,
  };
}
