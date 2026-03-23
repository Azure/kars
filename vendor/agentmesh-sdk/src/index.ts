/**
 * AgentMesh SDK - P2P Encrypted Messenger for AI Agents
 *
 * This module provides the client-side implementation of the AgentMesh protocol,
 * enabling AI agents to securely discover, authenticate, and communicate with
 * other agents peer-to-peer.
 *
 * @example
 * ```typescript
 * import { AgentMeshClient } from '@agentmesh/sdk';
 *
 * const client = await AgentMeshClient.create();
 * await client.connect({ capabilities: ['travel/flights'] });
 *
 * // Search for agents with a capability
 * const agents = await client.search('weather/forecast');
 *
 * // Send a message
 * await client.send(agents[0].amid, {
 *   intent: 'weather/forecast',
 *   message: { location: 'Berlin' }
 * });
 * ```
 *
 * @packageDocumentation
 */

export const VERSION = '0.1.0';
export const PROTOCOL_VERSION = 'agentmesh/0.2';

// Core Identity
export { Identity } from './identity';
export type { IdentityData, PublicInfo } from './identity';

// Discovery
export { RegistryClient } from './discovery';
export type { AgentInfo, RegisterOptions, SearchOptions, PrekeyBundle as RegistryPrekeyBundle } from './discovery';

// Transport
export { RelayTransport, P2PTransport, createP2PTransport } from './transport';
export type { RelayMessage, TransportOptions } from './transport';

// Storage
export { MemoryStorage, FileStorage, R2Storage, KVStorage } from './storage/index';
export type { Storage, StorageSetOptions } from './storage/index';

// Config
export { Policy, Config, Tier, TierLevel, getTierLevel, FileConfigLoader, ConfigError, createFileConfigLoader } from './config';
export type { PolicyOptions, ConfigOptions, KnockContext, PolicyResult, FileConfigOptions, PersistedSessionState, FileConfigEventType } from './config';

// Encryption
export {
  // X3DH
  X3DHKeyExchange,
  serializeX3DHMessage,
  deserializeX3DHMessage,
  // Prekeys
  PrekeyManager,
  generateX25519Keypair,
  generateSignedPrekey,
  generateOneTimePrekeys,
  serializePrekeyBundle,
  deserializePrekeyBundle,
  x25519DH,
  PREKEY_CONFIG,
  // Double Ratchet
  DoubleRatchetSession,
  serializeRatchetHeader,
  deserializeRatchetHeader,
  // Session Management
  SessionManager,
  SessionState,
  // HKDF
  hkdf,
  hkdfSimple,
  kdfRK,
  kdfCK,
} from './encryption/index';

export type {
  // X3DH types
  X3DHInitiatorMessage,
  X3DHInitiatorMessageSerialized,
  X3DHInitiatorResult,
  X3DHResponderResult,
  // Prekey types
  PrekeyBundle,
  PrekeyBundleSerialized,
  PrekeyState,
  OneTimePrekey,
  SignedPrekey,
  // Double Ratchet types
  DoubleRatchetState,
  DoubleRatchetStateSerialized,
  RatchetHeader,
  RatchetHeaderSerialized,
  EncryptedMessage,
  // Session types
  SessionInfo,
  SessionConfig,
  MessageEnvelope,
} from './encryption/index';

// Errors
export {
  AgentMeshError,
  CryptoError,
  NetworkError,
  SessionError,
  ValidationError,
  StorageError,
} from './errors';

// Client (High-Level API)
export { AgentMeshClient, CircuitState } from './client';
export type {
  ConnectOptions,
  SendOptions,
  MessageHandler,
  KnockHandler,
  ClientEventType,
  ClientInfo,
  ClientOptions,
} from './client';

// Certificates
export {
  CertificateManager,
  parsePEM,
  toPEM,
  parseCertificate,
  createTrustStore,
} from './certs';
export type {
  Certificate,
  ChainValidationResult,
  TrustStore,
} from './certs';

// Session/KNOCK Protocol
export {
  KnockProtocol,
  ProtocolSessionManager,
  SessionStateType,
  serializeIntentToJSON,
  deserializeIntentFromJSON,
} from './session';
export type {
  Intent,
  SessionRequest,
  SessionState as ProtocolSessionState,
  KnockMessage,
  KnockResponse,
} from './session';

// Schemas
export {
  SchemaValidator,
  CapabilityNegotiator,
  SequenceTracker,
  createValidator,
  BUILTIN_SCHEMAS,
} from './schemas';
export type {
  JSONSchema,
  Capability,
  CapabilityMatch,
  MessageSequence,
  SequenceStep,
} from './schemas';

// DID
export {
  DIDManager,
  DIDResolver,
  createRelayServiceEndpoint,
  createDHTServiceEndpoint,
} from './did';
export type {
  DIDDocument,
  DIDDocumentMetadata,
  VerificationMethod,
  ServiceEndpoint,
  SignedDIDDocument,
  DIDResolutionResult,
} from './did';

// DHT
export {
  DHTClient,
  KBucket,
  xorDistance,
  compareDistance,
  getBucketIndex,
  createCapabilityKey,
  createAmidKey,
} from './dht';
export type {
  DHTNode,
  DHTEntry,
  DHTCapabilityEntry,
  DHTMetrics,
} from './dht';

// Audit
export {
  AuditLogger,
  createAuditLogger,
  EncryptedAuditLogger,
  createEncryptedAuditLogger,
} from './audit';
export type {
  AuditEvent,
  AuditEventType,
  AuditSeverity,
  AuditQueryOptions,
  AuditLoggerConfig,
  EncryptedAuditEntry,
  EncryptedAuditLoggerConfig,
} from './audit';

// Rate Limiting
export {
  RateLimiter,
  RateLimitError,
} from './rate-limiter';
export type {
  RateLimitConfig,
  RateLimitStatus,
} from './rate-limiter';

// Session Cache
export { SessionCache } from './session/cache';
export type { SessionCacheConfig, CacheStats } from './session/cache';

// Dashboard
export { Dashboard, DashboardError } from './dashboard';
export type { DashboardConfig, DashboardEventType } from './dashboard';
