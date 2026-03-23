/**
 * Encryption module for AgentMesh.
 * Implements X3DH key exchange and Double Ratchet for E2E encryption.
 */

// HKDF key derivation
export { hkdf, hkdfSimple, kdfRK, kdfCK } from './hkdf';

// X3DH key exchange
export {
  X3DHKeyExchange,
  serializeX3DHMessage,
  deserializeX3DHMessage,
  type X3DHInitiatorMessage,
  type X3DHInitiatorMessageSerialized,
  type X3DHInitiatorResult,
  type X3DHResponderResult,
} from './x3dh';

// Prekey management
export {
  PrekeyManager,
  generateX25519Keypair,
  generateSignedPrekey,
  generateOneTimePrekeys,
  serializePrekeyBundle,
  deserializePrekeyBundle,
  x25519DH,
  PREKEY_CONFIG,
  type PrekeyBundle,
  type PrekeyBundleSerialized,
  type PrekeyState,
  type OneTimePrekey,
  type SignedPrekey,
} from './prekey';

// Double Ratchet
export {
  DoubleRatchetSession,
  serializeRatchetHeader,
  deserializeRatchetHeader,
  type DoubleRatchetState,
  type DoubleRatchetStateSerialized,
  type RatchetHeader,
  type RatchetHeaderSerialized,
  type EncryptedMessage,
} from './ratchet';

// Session management
export {
  SessionManager,
  SessionState,
  type SessionInfo,
  type SessionConfig,
  type MessageEnvelope,
} from './session';
