/**
 * AgentMeshClient - High-level API for AgentMesh.
 * Provides a simple interface for agent-to-agent communication.
 */

import { Identity } from './identity';
import { RegistryClient, AgentInfo, RegisterOptions, PrekeyBundle as RegistryPrekeyBundle } from './discovery';
import { RelayTransport, TransportOptions } from './transport';
import { SessionManager, SessionConfig, MessageEnvelope } from './encryption/session';
import { PrekeyManager, serializePrekeyBundle, PrekeyBundle } from './encryption/prekey';
import { serializeX3DHMessage, deserializeX3DHMessage, X3DHInitiatorMessage } from './encryption/x3dh';
import { Policy } from './config';
import { KnockProtocol, ProtocolSessionManager, SessionRequest, SessionStateType, SessionState as ProtocolSessionState } from './session';
import { AuditLogger, createAuditLogger } from './audit';
import { MemoryStorage, Storage } from './storage';
import { AgentMeshError, NetworkError, SessionError } from './errors';
import { RateLimiter, RateLimitConfig, RateLimitStatus, RateLimitError } from './rate-limiter';
import { SessionCache, SessionCacheConfig, CacheStats } from './session/cache';

/**
 * Client connection options.
 */
export interface ConnectOptions {
  /** Capabilities to register */
  capabilities?: string[];
  /** Display name */
  displayName?: string;
  /** Auto-upload prekeys on connect */
  autoUploadPrekeys?: boolean;
  /** Policy for KNOCK evaluation */
  policy?: Policy;
}

/**
 * Message options.
 */
export interface SendOptions {
  /** Intent for the message */
  intent?: string;
  /** Message priority */
  priority?: number;
  /** Request session type */
  sessionType?: 'one-shot' | 'streaming' | 'persistent';
  /** Session TTL in seconds */
  ttl?: number;
  /** Skip encryption (use with caution) */
  unencrypted?: boolean;
  /** Force optimistic send (KNOCK with message) */
  forceOptimistic?: boolean;
}

/**
 * Message handler type.
 */
export type MessageHandler = (
  from: string,
  message: unknown,
  envelope: MessageEnvelope
) => void | Promise<void>;

/**
 * KNOCK handler type.
 */
export type KnockHandler = (
  from: string,
  request: SessionRequest
) => Promise<{ accept: boolean; reason?: string }>;

/**
 * Event types.
 */
export type ClientEventType =
  | 'connected'
  | 'disconnected'
  | 'error'
  | 'message'
  | 'knock'
  | 'session_established'
  | 'session_closed'
  | 'session_killed'
  | 'peer_blocked'
  | 'peer_unblocked'
  | 'circuit_paused'
  | 'circuit_resumed'
  | 'emergency_stop'
  | 'rate_limited'
  | 'optimistic_dropped'
  | 'session_cached'
  | 'cache_cleared';

/**
 * Circuit breaker state.
 */
export enum CircuitState {
  RUNNING = 'RUNNING',
  PAUSED = 'PAUSED',
  STOPPED = 'STOPPED',
}

/**
 * Event handler type.
 */
export type EventHandler<T = unknown> = (event: T) => void;

/**
 * Client status information.
 */
export interface ClientInfo {
  /** Our AMID */
  amid: string;
  /** Connection status */
  connected: boolean;
  /** Registered capabilities */
  capabilities: string[];
  /** Active session count */
  activeSessions: number;
  /** Registry URL */
  registryUrl: string;
  /** Relay URL */
  relayUrl: string;
  /** Circuit breaker state */
  circuitState: CircuitState;
  /** Last circuit state change timestamp */
  circuitStateChangedAt: number;
}

/**
 * Client creation options.
 */
export interface ClientOptions {
  /** Registry URL */
  registryUrl?: string;
  /** Relay URL */
  relayUrl?: string;
  /** Storage backend */
  storage?: Storage;
  /** Session configuration */
  sessionConfig?: SessionConfig;
  /** Rate limiting configuration */
  rateLimit?: RateLimitConfig;
  /** Session cache configuration */
  sessionCache?: SessionCacheConfig;
  /** Enable optimistic send for allowlisted peers */
  optimisticSend?: boolean;
  /** Allowlist of AMIDs for optimistic send */
  optimisticAllowlist?: string[];
}

/**
 * AgentMesh Client for high-level agent communication.
 */
export class AgentMeshClient {
  private identity: Identity;
  private registry: RegistryClient;
  private transport: RelayTransport;
  private sessionManager: SessionManager;
  private prekeyManager: PrekeyManager;
  private protocolSessions: ProtocolSessionManager;
  private knockProtocol: KnockProtocol;
  private auditLogger: AuditLogger;
  private storage: Storage;
  private registryUrl: string;
  private relayUrl: string;

  private capabilities: string[] = [];
  private policy?: Policy;
  private connected: boolean = false;
  private activeSessions: Map<string, string> = new Map(); // peerAmid -> sessionId

  private messageHandlers: MessageHandler[] = [];
  private knockHandler?: KnockHandler;
  private eventHandlers: Map<ClientEventType, EventHandler[]> = new Map();

  // Circuit breaker state
  private circuitState: CircuitState = CircuitState.RUNNING;
  private circuitStateChangedAt: number = Date.now();
  private blocklist: Set<string> = new Set();

  // Rate limiting
  private rateLimiter?: RateLimiter;

  // Optimistic send
  private optimisticSendEnabled: boolean = false;
  private optimisticAllowlist: Set<string> = new Set();

  // Session cache
  private sessionCache: SessionCache;

  // Pending X3DH initiator messages (sent with first encrypted message to each peer)
  private pendingX3DH: Map<string, X3DHInitiatorMessage> = new Map();

  private constructor(identity: Identity, options: ClientOptions = {}) {
    this.identity = identity;
    this.storage = options.storage || new MemoryStorage();
    this.registryUrl = options.registryUrl || 'https://agentmesh.online/v1';
    this.relayUrl = options.relayUrl || 'wss://relay.agentmesh.online/v1/connect';

    // Initialize components
    this.registry = new RegistryClient(this.registryUrl);

    const transportOptions: TransportOptions = {
      relayUrl: this.relayUrl,
    };
    this.transport = new RelayTransport(identity, transportOptions);

    this.prekeyManager = new PrekeyManager(identity, this.storage);
    this.sessionManager = new SessionManager(identity, this.storage, this.prekeyManager, options.sessionConfig);
    this.protocolSessions = new ProtocolSessionManager();
    this.knockProtocol = new KnockProtocol(identity);
    this.auditLogger = createAuditLogger(identity.amid);

    // Initialize rate limiter if configured
    if (options.rateLimit) {
      this.rateLimiter = new RateLimiter(options.rateLimit);
      this.rateLimiter.onEvent(event => {
        if (event.type === 'rate_limited') {
          this.emitEvent('rate_limited', event.data);
        }
      });
    }

    // Initialize optimistic send
    this.optimisticSendEnabled = options.optimisticSend ?? false;
    if (options.optimisticAllowlist) {
      for (const amid of options.optimisticAllowlist) {
        this.optimisticAllowlist.add(amid);
      }
    }

    // Initialize session cache
    this.sessionCache = new SessionCache(options.sessionCache);

    // Listen for optimistic_dropped events from transport
    this.transport.onTransportEvent('optimistic_dropped', (data) => {
      this.emitEvent('optimistic_dropped', data);
    });
  }

  /**
   * Create a new client with a generated identity.
   */
  static async create(options?: ClientOptions): Promise<AgentMeshClient> {
    const identity = await Identity.generate();
    return new AgentMeshClient(identity, options || {});
  }

  /**
   * Load a client from storage.
   */
  static async load(
    storage: Storage,
    path: string = 'identity',
    options?: Omit<ClientOptions, 'storage'>
  ): Promise<AgentMeshClient> {
    const identity = await Identity.load(storage, path);
    return new AgentMeshClient(identity, { storage, ...options });
  }

  /**
   * Create a client from an existing identity.
   */
  static fromIdentity(identity: Identity, options?: ClientOptions): AgentMeshClient {
    return new AgentMeshClient(identity, options);
  }

  /**
   * Get the underlying identity.
   */
  getIdentity(): Identity {
    return this.identity;
  }

  /**
   * Get the client's AMID.
   */
  get amid(): string {
    return this.identity.amid;
  }

  /**
   * Connect to the AgentMesh network.
   */
  async connect(options: ConnectOptions = {}): Promise<void> {
    if (this.connected) {
      throw new AgentMeshError('Already connected', 'ALREADY_CONNECTED');
    }

    // Store config
    this.capabilities = options.capabilities || [];
    this.policy = options.policy;

    if (this.policy) {
      this.knockProtocol.setPolicy(this.policy);
    }

    // Load or generate prekeys
    await this.prekeyManager.loadOrInitialize();

    // Register with the registry
    const registerOptions: RegisterOptions = {
      displayName: options.displayName,
      capabilities: this.capabilities,
    };

    await this.registry.register(this.identity, registerOptions);

    // Upload prekeys
    if (options.autoUploadPrekeys !== false) {
      await this.uploadPrekeys();
    }

    // Connect transport
    await this.transport.connect();

    // Wire relay transport 'receive' events to client handlers.
    // This is the critical bridge: the relay delivers messages as 'receive' type,
    // and we route them to KNOCK handlers and message handlers.
    this.transport.onMessage('receive', async (data: Record<string, unknown>) => {
      const fromAmid = data.from as string;
      const rawPayload = data.encrypted_payload as string;
      const msgType = data.message_type as string;

      try {
        const parsed = JSON.parse(rawPayload);

        if (msgType === 'knock') {
          // Incoming KNOCK — evaluate and auto-accept/reject
          const request = parsed.request || parsed;
          const result = await this.handleIncomingKnock(fromAmid, request);
          if (result.accept) {
            // Send ACCEPT response back via relay
            try {
              const accept = await this.knockProtocol.createAcceptResponse(parsed, result.sessionId);
              await this.transport.send(fromAmid, JSON.stringify(accept), 'accept' as any);
            } catch { /* best effort */ }
          }
        } else if (parsed.type === 'encrypted' && parsed.x3dh) {
          // First encrypted message with X3DH params — establish responder session
          try {
            const x3dhMsg = deserializeX3DHMessage(parsed.x3dh);
            const sessionId = await this.sessionManager.acceptSession(fromAmid, x3dhMsg);
            this.activeSessions.set(fromAmid, sessionId);
            // Decrypt with the newly established session
            const decrypted = await this.sessionManager.decryptMessage(sessionId, parsed);
            for (const handler of this.messageHandlers) {
              try { handler(fromAmid, decrypted); } catch { /* handler error */ }
            }
          } catch (e: any) {
            // Decryption failed — deliver the raw parsed object
            for (const handler of this.messageHandlers) {
              try { handler(fromAmid, parsed); } catch { /* handler error */ }
            }
          }
        } else if (parsed.type === 'encrypted') {
          // Subsequent encrypted message — decrypt with existing session
          const sessionId = this.activeSessions.get(fromAmid);
          if (sessionId) {
            try {
              const decrypted = await this.sessionManager.decryptMessage(sessionId, parsed);
              for (const handler of this.messageHandlers) {
                try { handler(fromAmid, decrypted); } catch { /* handler error */ }
              }
            } catch {
              for (const handler of this.messageHandlers) {
                try { handler(fromAmid, parsed); } catch { /* handler error */ }
              }
            }
          } else {
            for (const handler of this.messageHandlers) {
              try { handler(fromAmid, parsed); } catch { /* handler error */ }
            }
          }
        } else {
          // Unencrypted/plain message — deliver as-is
          for (const handler of this.messageHandlers) {
            try { handler(fromAmid, parsed); } catch { /* handler error */ }
          }
        }
      } catch {
        // JSON parse failed — deliver raw payload
        for (const handler of this.messageHandlers) {
          try { handler(fromAmid, rawPayload); } catch { /* handler error */ }
        }
      }
    });

    this.connected = true;

    // Emit connected event
    this.emitEvent('connected', { amid: this.amid });

    // Log
    await this.auditLogger.log('CONNECTION_ESTABLISHED', 'INFO', 'Connected to AgentMesh');
  }

  /**
   * Disconnect from the AgentMesh network.
   */
  async disconnect(): Promise<void> {
    if (!this.connected) return;

    // Update status to offline
    try {
      await this.registry.updateStatus(this.identity, 'offline');
    } catch {
      // Ignore errors during disconnect
    }

    // Disconnect transport
    await this.transport.disconnect('Client disconnect');

    this.connected = false;

    // Emit disconnected event
    this.emitEvent('disconnected', { amid: this.amid });

    // Log
    await this.auditLogger.log('CONNECTION_LOST', 'INFO', 'Disconnected from AgentMesh');
  }

  /**
   * Check if connected.
   */
  get isConnected(): boolean {
    return this.connected && this.transport.isConnected;
  }

  /**
   * Search for agents with a capability.
   */
  async search(
    capability: string,
    options?: { limit?: number; tierMin?: number }
  ): Promise<AgentInfo[]> {
    const result = await this.registry.search({
      capability,
      limit: options?.limit,
      tierMin: options?.tierMin,
    });
    return result.results;
  }

  /**
   * Look up an agent by AMID.
   */
  async lookup(amid: string): Promise<AgentInfo | null> {
    return this.registry.lookup(amid);
  }

  /**
   * Send a message to an agent.
   */
  async send(
    toAmid: string,
    message: Record<string, unknown>,
    options: SendOptions = {}
  ): Promise<void> {
    if (this.circuitState === CircuitState.STOPPED) {
      throw new AgentMeshError('Client is stopped', 'CLIENT_STOPPED');
    }

    if (!this.connected) {
      throw new NetworkError('Not connected', 'NOT_CONNECTED');
    }

    if (this.isBlocked(toAmid)) {
      throw new SessionError(`Peer ${toAmid} is blocked`, 'PEER_BLOCKED');
    }

    // Check rate limit (bypass for control messages handled separately)
    if (this.rateLimiter) {
      this.rateLimiter.consume(toAmid);
    }

    // Check for existing session in active sessions
    let sessionId = this.activeSessions.get(toAmid);
    const intent = options.intent || '*';

    if (!sessionId) {
      // Check session cache for cache hit (skip KNOCK protocol)
      const cachedSession = this.sessionCache.get(this.amid, toAmid, intent);
      if (cachedSession) {
        // Cache hit - use cached session, skip KNOCK
        sessionId = cachedSession.sessionId;
        this.activeSessions.set(toAmid, sessionId);

        // Log cache hit
        await this.auditLogger.log('SESSION_CACHED', 'INFO', `Cache hit for ${toAmid}`);
      } else {
        // Cache miss - need to establish a new session
        sessionId = await this.establishSession(toAmid, options);

        // Store in cache for future use
        const ttlMs = (options.ttl || 3600) * 1000; // Convert seconds to ms
        this.sessionCache.set(sessionId, this.amid, toAmid, intent, ttlMs);
      }
    }

    if (options.unencrypted) {
      // Send without encryption
      await this.transport.send(toAmid, JSON.stringify(message), 'message');
    } else {
      // Encrypt and send
      const envelope: Record<string, unknown> = await this.sessionManager.encryptMessage(sessionId, message);

      // Attach X3DH initiator params to first message so receiver can establish session
      const x3dhMsg = this.pendingX3DH.get(toAmid);
      if (x3dhMsg) {
        envelope.x3dh = serializeX3DHMessage(x3dhMsg);
        this.pendingX3DH.delete(toAmid);
      }

      await this.transport.send(toAmid, JSON.stringify(envelope), 'message');
    }

    // Update session tracking
    const protocolSession = this.protocolSessions.getSessionsForPeer(toAmid)[0];
    if (protocolSession) {
      this.protocolSessions.recordMessageSent(protocolSession.id);
    }

    // Log
    await this.auditLogger.logMessageSent(toAmid, sessionId);
  }

  /**
   * Establish a session with an agent via KNOCK protocol.
   *
   * Flow: X3DH key exchange → send KNOCK via relay → activate session
   *
   * Note: Full bidirectional KNOCK (send KNOCK, wait for ACCEPT) requires
   * the receiving agent to process relay "receive" messages and respond.
   * The SDK transport currently doesn't wire relay messages back to the client's
   * KNOCK handler automatically. We send the KNOCK for protocol compliance
   * and activate the session after X3DH succeeds — the shared secret is
   * established by the key exchange, so the initiator can encrypt immediately.
   * The responder will process the KNOCK asynchronously.
   */
  private async establishSession(toAmid: string, options: SendOptions): Promise<string> {
    // Get recipient's prekey bundle from registry
    const registryBundle = await this.registry.getPrekeys(toAmid);
    if (!registryBundle) {
      throw new SessionError(`Cannot get prekeys for ${toAmid}`, 'PREKEY_NOT_FOUND');
    }

    // Get recipient's public key for verification
    const agentInfo = await this.registry.lookup(toAmid);
    if (!agentInfo) {
      throw new SessionError(`Cannot find agent ${toAmid}`, 'AGENT_NOT_FOUND');
    }

    // Convert registry bundle to encryption bundle format
    const bundle = this.convertRegistryBundle(registryBundle);

    // Decode signing public key from base64
    const signingKeyB64 = agentInfo.signingPublicKey;
    const signingKey = this.base64Decode(signingKeyB64);

    // Initialize encrypted session (X3DH key exchange — shared secret established)
    const { sessionId, x3dhMessage } = await this.sessionManager.initiateSession(
      toAmid,
      bundle,
      signingKey
    );

    // Store X3DH initiator params for inclusion in first encrypted message
    // (the receiver needs these to establish the responder side of the session)
    this.pendingX3DH.set(toAmid, x3dhMessage);

    // Store session mapping
    this.activeSessions.set(toAmid, sessionId);

    // Create KNOCK request
    const request: SessionRequest = {
      type: options.sessionType || 'one-shot',
      ttl: options.ttl || 3600,
      intent: {
        capability: options.intent || '*',
        action: 'message',
      },
      priority: options.priority,
    };

    // Send KNOCK via relay for protocol compliance
    try {
      const knock = await this.knockProtocol.createKnock(toAmid, request);
      await this.transport.send(toAmid, JSON.stringify(knock), 'knock');
    } catch {
      // KNOCK send failure is non-fatal — X3DH already established the shared secret
    }

    // Activate session — X3DH completed with peer's signed prekey as ratchet key
    await this.sessionManager.activateSessionDirect(sessionId);

    this.protocolSessions.createSession(toAmid, request, true);

    // Log
    await this.auditLogger.logSessionInitiated(toAmid, sessionId);

    return sessionId;
  }

  /**
   * Convert registry prekey bundle to encryption module format.
   */
  private convertRegistryBundle(registry: RegistryPrekeyBundle): PrekeyBundle {
    return {
      identityKey: this.base64Decode(registry.identityKey),
      signedPrekey: this.base64Decode(registry.signedPrekey),
      signedPrekeySignature: this.base64Decode(registry.signedPrekeySignature),
      signedPrekeyId: registry.signedPrekeyId,
      oneTimePrekeys: registry.oneTimePrekeys.map(otp => ({
        id: otp.id,
        key: this.base64Decode(otp.key),
      })),
    };
  }

  /**
   * Register a message handler.
   */
  onMessage(handler: MessageHandler): void {
    this.messageHandlers.push(handler);
  }

  /**
   * Register a KNOCK handler.
   */
  onKnock(handler: KnockHandler): void {
    this.knockHandler = handler;
  }

  /**
   * Handle an incoming KNOCK request.
   * This method checks circuit state before policy evaluation.
   */
  async handleIncomingKnock(
    fromAmid: string,
    request: SessionRequest
  ): Promise<{ accept: boolean; reason?: string; sessionId?: string }> {
    // Check circuit breaker state first
    if (this.circuitState === CircuitState.STOPPED) {
      return { accept: false, reason: 'agent_stopped' };
    }

    if (this.circuitState === CircuitState.PAUSED) {
      return { accept: false, reason: 'agent_paused' };
    }

    // Check blocklist
    if (this.isBlocked(fromAmid)) {
      return { accept: false, reason: 'blocked' };
    }

    // Use custom KNOCK handler if set
    if (this.knockHandler) {
      const result = await this.knockHandler(fromAmid, request);
      if (!result.accept) {
        await this.auditLogger.log('KNOCK_RECEIVED', 'INFO',
          `KNOCK rejected from ${fromAmid}: ${result.reason || 'custom_handler'}`);
        return { accept: false, reason: result.reason || 'rejected' };
      }
    }

    // Evaluate policy
    if (this.policy) {
      const context = {
        fromAmid,
        fromTier: 'anonymous', // Would need registry lookup for real tier
        fromReputation: 0,
        intentCategory: request.intent.capability,
        requestedTtl: request.ttl,
      };

      const policyResult = this.policy.evaluate(context);
      if (!policyResult.allowed) {
        await this.auditLogger.log('KNOCK_RECEIVED', 'INFO',
          `KNOCK rejected from ${fromAmid}: ${policyResult.reason || 'policy'}`);
        return { accept: false, reason: policyResult.reason || 'policy_rejected' };
      }
    }

    // Accept - create session
    const sessionId = `sess_${Date.now()}_${Math.random().toString(36).slice(2)}`;
    this.protocolSessions.createSession(fromAmid, request, false);

    await this.auditLogger.log('KNOCK_RECEIVED', 'INFO',
      `KNOCK accepted from ${fromAmid}, session: ${sessionId}`);

    this.emitEvent('knock', { fromAmid, request, sessionId });
    this.emitEvent('session_established', { amid: fromAmid, sessionId });

    return { accept: true, sessionId };
  }

  /**
   * Register an event handler.
   */
  on(event: ClientEventType, handler: EventHandler): void {
    const handlers = this.eventHandlers.get(event) || [];
    handlers.push(handler);
    this.eventHandlers.set(event, handlers);
  }

  /**
   * Remove an event handler.
   */
  off(event: ClientEventType, handler: EventHandler): void {
    const handlers = this.eventHandlers.get(event) || [];
    const index = handlers.indexOf(handler);
    if (index !== -1) {
      handlers.splice(index, 1);
    }
  }

  /**
   * Set the policy for KNOCK evaluation.
   */
  setPolicy(policy: Policy): void {
    this.policy = policy;
    this.knockProtocol.setPolicy(policy);
  }

  /**
   * Update registered capabilities.
   */
  async setCapabilities(capabilities: string[]): Promise<void> {
    this.capabilities = capabilities;
    if (this.connected) {
      await this.registry.updateCapabilities(this.identity, capabilities);
    }
  }

  /**
   * Get all active sessions.
   */
  getSessions(): ProtocolSessionState[] {
    return this.protocolSessions.getActiveSessions();
  }

  /**
   * Get a specific session.
   */
  getSession(amid: string): ProtocolSessionState | undefined {
    const sessions = this.protocolSessions.getSessionsForPeer(amid);
    return sessions.find(s => s.state === SessionStateType.ACTIVE);
  }

  /**
   * Close a session.
   */
  async closeSession(amid: string): Promise<void> {
    const sessions = this.protocolSessions.getSessionsForPeer(amid);
    for (const session of sessions) {
      this.protocolSessions.closeSession(session.id);
    }

    const sessionId = this.activeSessions.get(amid);
    if (sessionId) {
      this.sessionManager.closeSession(sessionId);
      this.activeSessions.delete(amid);
    }

    // Clear cached sessions for this peer
    this.sessionCache.clearByAmid(amid);

    this.emitEvent('session_closed', { amid });
  }

  // ========== CIRCUIT BREAKERS ==========

  /**
   * Kill a session with a specific peer immediately.
   */
  async killSession(amid: string): Promise<void> {
    const sessions = this.protocolSessions.getSessionsForPeer(amid);
    for (const session of sessions) {
      this.protocolSessions.closeSession(session.id);
    }

    const sessionId = this.activeSessions.get(amid);
    if (sessionId) {
      this.sessionManager.closeSession(sessionId);
      this.activeSessions.delete(amid);
    }

    // Send CLOSE message to peer
    if (this.isConnected) {
      try {
        await this.transport.send(amid, JSON.stringify({ type: 'close', reason: 'session_killed' }), 'close');
      } catch {
        // Ignore send errors during kill
      }
    }

    this.emitEvent('session_killed', { amid });
    await this.auditLogger.log('SESSION_CLOSED', 'INFO', `Session killed for ${amid}`);
  }

  /**
   * Pause accepting new KNOCK requests.
   */
  pauseNew(): void {
    if (this.circuitState === CircuitState.STOPPED) {
      throw new AgentMeshError('Client is stopped', 'CLIENT_STOPPED');
    }

    if (this.circuitState !== CircuitState.PAUSED) {
      this.circuitState = CircuitState.PAUSED;
      this.circuitStateChangedAt = Date.now();
      this.emitEvent('circuit_paused', { timestamp: this.circuitStateChangedAt });
    }
  }

  /**
   * Resume accepting new KNOCK requests.
   */
  resumeNew(): void {
    if (this.circuitState === CircuitState.STOPPED) {
      throw new AgentMeshError('Client is stopped', 'CLIENT_STOPPED');
    }

    if (this.circuitState === CircuitState.PAUSED) {
      this.circuitState = CircuitState.RUNNING;
      this.circuitStateChangedAt = Date.now();
      this.emitEvent('circuit_resumed', { timestamp: this.circuitStateChangedAt });
    }
  }

  /**
   * Block a peer and kill their session.
   */
  async block(amid: string): Promise<void> {
    this.blocklist.add(amid);

    // Kill any existing session
    await this.killSession(amid);

    // Clear cached sessions for this peer
    this.sessionCache.clearByAmid(amid);

    this.emitEvent('peer_blocked', { amid });
    await this.auditLogger.log('POLICY_EVALUATED', 'WARNING', `Peer blocked: ${amid}`);
  }

  /**
   * Unblock a peer.
   */
  async unblock(amid: string): Promise<void> {
    this.blocklist.delete(amid);
    this.emitEvent('peer_unblocked', { amid });
  }

  /**
   * Emergency stop - disconnect, reject all, clear sessions.
   * This is a terminal state.
   */
  async emergencyStop(): Promise<void> {
    this.circuitState = CircuitState.STOPPED;
    this.circuitStateChangedAt = Date.now();

    // Close all sessions
    for (const amid of this.activeSessions.keys()) {
      await this.killSession(amid);
    }

    // Disconnect from relay
    if (this.connected) {
      await this.disconnect();
    }

    this.emitEvent('emergency_stop', { timestamp: this.circuitStateChangedAt });
    await this.auditLogger.log('CONNECTION_LOST', 'ERROR', 'Emergency stop triggered');
  }

  /**
   * Get current circuit breaker state.
   */
  getCircuitState(): { state: CircuitState; changedAt: number } {
    return {
      state: this.circuitState,
      changedAt: this.circuitStateChangedAt,
    };
  }

  /**
   * Check if a peer is blocked.
   */
  isBlocked(amid: string): boolean {
    return this.blocklist.has(amid);
  }

  // ========== END CIRCUIT BREAKERS ==========

  // ========== RATE LIMITING ==========

  /**
   * Get rate limit status.
   */
  getRateLimitStatus(): RateLimitStatus | null {
    if (!this.rateLimiter) {
      return null;
    }
    return this.rateLimiter.getStatus();
  }

  /**
   * Check if rate limit allows sending.
   */
  canSend(peerAmid?: string): boolean {
    if (!this.rateLimiter) {
      return true;
    }
    return this.rateLimiter.canConsume(peerAmid);
  }

  /**
   * Wait for rate limit capacity.
   */
  async waitForSendCapacity(maxWaitMs: number, peerAmid?: string): Promise<boolean> {
    if (!this.rateLimiter) {
      return true;
    }
    return this.rateLimiter.waitForCapacity(maxWaitMs, peerAmid);
  }

  // ========== END RATE LIMITING ==========

  // ========== SESSION CACHE ==========

  /**
   * Get session cache statistics.
   */
  getCacheStats(): CacheStats {
    return this.sessionCache.getStats();
  }

  /**
   * Clear cached session for a specific peer and intent.
   */
  clearCachedSession(amid: string, intent: string): boolean {
    const cleared = this.sessionCache.clear(this.amid, amid, intent);
    if (cleared) {
      this.emitEvent('cache_cleared', { amid, intent });
    }
    return cleared;
  }

  /**
   * Clear all cached sessions for a peer.
   */
  clearCachedSessionsForPeer(amid: string): number {
    const count = this.sessionCache.clearByAmid(amid);
    if (count > 0) {
      this.emitEvent('cache_cleared', { amid, count });
    }
    return count;
  }

  /**
   * Clear all cached sessions.
   */
  clearAllCachedSessions(): void {
    this.sessionCache.clearAll();
    this.emitEvent('cache_cleared', { all: true });
  }

  /**
   * Get all cached sessions (for dashboard).
   */
  getCachedSessions(): { sessionId: string; peerAmid: string; expiresAt: number }[] {
    return this.sessionCache.getAll().map(s => ({
      sessionId: s.sessionId,
      peerAmid: s.receiverAmid === this.amid ? s.initiatorAmid : s.receiverAmid,
      expiresAt: s.expiresAt,
    }));
  }

  // ========== END SESSION CACHE ==========

  // ========== OPTIMISTIC SEND ==========

  /**
   * Add a peer to the optimistic send allowlist.
   */
  addOptimisticPeer(amid: string): void {
    this.optimisticAllowlist.add(amid);
  }

  /**
   * Remove a peer from the optimistic send allowlist.
   */
  removeOptimisticPeer(amid: string): void {
    this.optimisticAllowlist.delete(amid);
  }

  /**
   * Check if a peer is in the optimistic send allowlist.
   */
  isOptimisticPeer(amid: string): boolean {
    return this.optimisticAllowlist.has(amid);
  }

  /**
   * Enable or disable optimistic send.
   */
  setOptimisticSend(enabled: boolean): void {
    this.optimisticSendEnabled = enabled;
  }

  /**
   * Check if optimistic send should be used for a peer.
   */
  private shouldUseOptimisticSend(toAmid: string, options: SendOptions): boolean {
    // Force optimistic if specified
    if (options.forceOptimistic) {
      return true;
    }

    // Use optimistic if enabled and peer is in allowlist
    return this.optimisticSendEnabled && this.optimisticAllowlist.has(toAmid);
  }

  // ========== END OPTIMISTIC SEND ==========

  /**
   * Get client information.
   */
  getInfo(): ClientInfo {
    return {
      amid: this.amid,
      connected: this.connected,
      capabilities: this.capabilities,
      activeSessions: this.protocolSessions.getActiveSessions().length,
      registryUrl: this.registryUrl,
      relayUrl: this.relayUrl,
      circuitState: this.circuitState,
      circuitStateChangedAt: this.circuitStateChangedAt,
    };
  }

  /**
   * Upload prekeys to registry.
   */
  async uploadPrekeys(): Promise<void> {
    const bundle = await this.prekeyManager.loadOrInitialize();
    const serialized = serializePrekeyBundle(bundle);
    await this.registry.uploadPrekeys(
      this.identity,
      serialized.signed_prekey,
      serialized.signed_prekey_signature,
      serialized.signed_prekey_id,
      serialized.one_time_prekeys
    );

    // Log
    await this.auditLogger.log('PREKEY_ROTATED', 'INFO', 'Prekeys uploaded');
  }

  /**
   * Rotate prekeys and upload to registry.
   */
  async rotatePrekeys(): Promise<void> {
    // Load fresh prekeys (which rotates if needed)
    await this.prekeyManager.loadOrInitialize();
    await this.uploadPrekeys();
  }

  /**
   * Save the client state to storage.
   */
  async save(path: string = 'identity'): Promise<void> {
    await this.identity.save(this.storage, path);
  }

  /**
   * Emit an event to handlers.
   */
  private emitEvent(event: ClientEventType, data: unknown): void {
    const handlers = this.eventHandlers.get(event) || [];
    for (const handler of handlers) {
      try {
        handler(data);
      } catch {
        // Ignore handler errors
      }
    }
  }

  /**
   * Base64 decode to bytes.
   */
  private base64Decode(b64: string): Uint8Array {
    // Strip key type prefixes (e.g. "ed25519:" or "x25519:") before decoding
    let raw = b64;
    if (raw.startsWith('ed25519:')) raw = raw.slice(8);
    else if (raw.startsWith('x25519:')) raw = raw.slice(7);
    const binary = atob(raw);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) {
      bytes[i] = binary.charCodeAt(i);
    }
    return bytes;
  }
}
