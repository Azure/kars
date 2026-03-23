/**
 * X3DH (Extended Triple Diffie-Hellman) Key Exchange.
 * Implements the Signal Protocol X3DH for establishing shared secrets.
 */

import { Identity } from '../identity';
import { hkdfSimple } from './hkdf';
import {
  x25519DH,
  generateX25519Keypair,
  PrekeyBundle,
  deserializePrekeyBundle,
} from './prekey';

/**
 * X3DH initiator message - sent with first message to establish session.
 */
export interface X3DHInitiatorMessage {
  /** Our identity public key (X25519) */
  identityKey: Uint8Array;
  /** Ephemeral public key for this session */
  ephemeralKey: Uint8Array;
  /** ID of the signed prekey we used */
  signedPrekeyId: number;
  /** ID of the one-time prekey we used (if any) */
  oneTimePrekeyId?: number;
}

/**
 * Serialized X3DH initiator message for wire transport.
 */
export interface X3DHInitiatorMessageSerialized {
  identity_key: string;
  ephemeral_key: string;
  signed_prekey_id: number;
  one_time_prekey_id?: number;
}

/**
 * Result of X3DH key exchange (initiator side).
 */
export interface X3DHInitiatorResult {
  /** The shared secret derived from X3DH */
  sharedSecret: Uint8Array;
  /** The initiator message to send to responder */
  initiatorMessage: X3DHInitiatorMessage;
  /** The ephemeral private key (for potential ratchet initialization) */
  ephemeralPrivate: Uint8Array;
}

/**
 * Result of X3DH key exchange (responder side).
 */
export interface X3DHResponderResult {
  /** The shared secret derived from X3DH */
  sharedSecret: Uint8Array;
  /** The identity public key of the initiator */
  initiatorIdentityKey: Uint8Array;
}

/**
 * X3DH Key Exchange implementation.
 *
 * The X3DH protocol establishes a shared secret between two parties:
 * - Alice (initiator) wants to establish a session with Bob (responder)
 * - Bob has published his prekey bundle to a registry
 * - Alice fetches Bob's prekey bundle and computes a shared secret
 *
 * The shared secret is computed as:
 * - DH1 = DH(IKa, SPKb) - Alice's identity key with Bob's signed prekey
 * - DH2 = DH(EKa, IKb) - Alice's ephemeral key with Bob's identity key
 * - DH3 = DH(EKa, SPKb) - Alice's ephemeral key with Bob's signed prekey
 * - DH4 = DH(EKa, OPKb) - Alice's ephemeral key with Bob's one-time prekey (optional)
 *
 * SK = HKDF(DH1 || DH2 || DH3 || DH4, "X3DH", 32)
 */
export class X3DHKeyExchange {
  /**
   * Perform X3DH as the initiator (Alice).
   *
   * @param ourIdentity - Our identity (provides exchange private key)
   * @param theirBundle - Their published prekey bundle
   * @param theirSigningPublicKey - Their signing public key (to verify prekey signature)
   * @returns The shared secret and initiator message
   */
  static async initiator(
    ourIdentity: Identity,
    theirBundle: PrekeyBundle,
    theirSigningPublicKey: Uint8Array
  ): Promise<X3DHInitiatorResult> {
    // Verify signed prekey signature
    const signatureValid = await Identity.verifySignatureRaw(
      theirSigningPublicKey,
      theirBundle.signedPrekey,
      theirBundle.signedPrekeySignature
    );

    if (!signatureValid) {
      throw new Error('Invalid signed prekey signature');
    }

    // Generate ephemeral keypair
    const ephemeral = await generateX25519Keypair();

    // Get our exchange private key
    const ourIdentityPrivate = ourIdentity.getExchangePrivateKeyRaw();

    // Compute DH values
    const dh1 = await x25519DH(ourIdentityPrivate, theirBundle.signedPrekey);
    const dh2 = await x25519DH(ephemeral.privateKey, theirBundle.identityKey);
    const dh3 = await x25519DH(ephemeral.privateKey, theirBundle.signedPrekey);

    // Concatenate DH outputs
    let dhConcat: Uint8Array;
    let oneTimePrekeyId: number | undefined;

    const oneTimePrekey = theirBundle.oneTimePrekeys[0];
    if (oneTimePrekey) {
      // Use first available one-time prekey
      oneTimePrekeyId = oneTimePrekey.id;

      const dh4 = await x25519DH(ephemeral.privateKey, oneTimePrekey.key);

      dhConcat = new Uint8Array(dh1.length + dh2.length + dh3.length + dh4.length);
      dhConcat.set(dh1, 0);
      dhConcat.set(dh2, dh1.length);
      dhConcat.set(dh3, dh1.length + dh2.length);
      dhConcat.set(dh4, dh1.length + dh2.length + dh3.length);
    } else {
      dhConcat = new Uint8Array(dh1.length + dh2.length + dh3.length);
      dhConcat.set(dh1, 0);
      dhConcat.set(dh2, dh1.length);
      dhConcat.set(dh3, dh1.length + dh2.length);
    }

    // Derive shared secret using HKDF
    const sharedSecret = await hkdfSimple(dhConcat, 'X3DH', 32);

    const initiatorMessage: X3DHInitiatorMessage = {
      identityKey: ourIdentity.getExchangePublicKeyRaw(),
      ephemeralKey: ephemeral.publicKey,
      signedPrekeyId: theirBundle.signedPrekeyId,
      oneTimePrekeyId,
    };

    return {
      sharedSecret,
      initiatorMessage,
      ephemeralPrivate: ephemeral.privateKey,
    };
  }

  /**
   * Perform X3DH as the responder (Bob).
   *
   * @param ourIdentity - Our identity (provides exchange private key)
   * @param ourSignedPrekeyPrivate - Our signed prekey private key
   * @param ourOneTimePrekeyPrivate - Our one-time prekey private key (if used)
   * @param initiatorMessage - The initiator's X3DH message
   * @returns The shared secret
   */
  static async responder(
    ourIdentity: Identity,
    ourSignedPrekeyPrivate: Uint8Array,
    ourOneTimePrekeyPrivate: Uint8Array | null,
    initiatorMessage: X3DHInitiatorMessage
  ): Promise<X3DHResponderResult> {
    const ourIdentityPrivate = ourIdentity.getExchangePrivateKeyRaw();

    // Compute DH values (reversed from initiator)
    const dh1 = await x25519DH(ourSignedPrekeyPrivate, initiatorMessage.identityKey);
    const dh2 = await x25519DH(ourIdentityPrivate, initiatorMessage.ephemeralKey);
    const dh3 = await x25519DH(ourSignedPrekeyPrivate, initiatorMessage.ephemeralKey);

    // Concatenate DH outputs
    let dhConcat: Uint8Array;

    if (ourOneTimePrekeyPrivate) {
      const dh4 = await x25519DH(ourOneTimePrekeyPrivate, initiatorMessage.ephemeralKey);

      dhConcat = new Uint8Array(dh1.length + dh2.length + dh3.length + dh4.length);
      dhConcat.set(dh1, 0);
      dhConcat.set(dh2, dh1.length);
      dhConcat.set(dh3, dh1.length + dh2.length);
      dhConcat.set(dh4, dh1.length + dh2.length + dh3.length);
    } else {
      dhConcat = new Uint8Array(dh1.length + dh2.length + dh3.length);
      dhConcat.set(dh1, 0);
      dhConcat.set(dh2, dh1.length);
      dhConcat.set(dh3, dh1.length + dh2.length);
    }

    // Derive shared secret using HKDF
    const sharedSecret = await hkdfSimple(dhConcat, 'X3DH', 32);

    return {
      sharedSecret,
      initiatorIdentityKey: initiatorMessage.identityKey,
    };
  }

  /**
   * Simple X25519 key exchange (fallback when no prekeys available).
   */
  static async simpleKeyExchange(
    ourPrivateKey: Uint8Array,
    theirPublicKey: Uint8Array
  ): Promise<Uint8Array> {
    return x25519DH(ourPrivateKey, theirPublicKey);
  }

  /**
   * Generate ephemeral keypair.
   */
  static async generateEphemeralKeypair(): Promise<{
    publicKey: Uint8Array;
    privateKey: Uint8Array;
  }> {
    return generateX25519Keypair();
  }
}

/**
 * Serialize X3DH initiator message for wire transport.
 */
export function serializeX3DHMessage(msg: X3DHInitiatorMessage): X3DHInitiatorMessageSerialized {
  const toBase64 = (bytes: Uint8Array) => {
    const binary = String.fromCharCode(...bytes);
    return btoa(binary);
  };

  return {
    identity_key: toBase64(msg.identityKey),
    ephemeral_key: toBase64(msg.ephemeralKey),
    signed_prekey_id: msg.signedPrekeyId,
    one_time_prekey_id: msg.oneTimePrekeyId,
  };
}

/**
 * Deserialize X3DH initiator message from wire transport.
 */
export function deserializeX3DHMessage(data: X3DHInitiatorMessageSerialized): X3DHInitiatorMessage {
  const fromBase64 = (b64: string): Uint8Array => {
    const binary = atob(b64);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) {
      bytes[i] = binary.charCodeAt(i);
    }
    return bytes;
  };

  return {
    identityKey: fromBase64(data.identity_key),
    ephemeralKey: fromBase64(data.ephemeral_key),
    signedPrekeyId: data.signed_prekey_id,
    oneTimePrekeyId: data.one_time_prekey_id,
  };
}
