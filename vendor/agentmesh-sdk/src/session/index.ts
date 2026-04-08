/**
 * Session module for AgentMesh.
 * Implements the KNOCK protocol for session establishment and management.
 */

import { Identity } from '../identity';

import type { Policy, KnockContext, PolicyResult } from '../config';
import type { Certificate, CertificateManager } from '../certs';

/**
 * Intent specification for a session request.
 */
export interface Intent {
  /** Capability being requested (e.g., 'weather/forecast') */
  capability: string;
  /** Action to perform (e.g., 'query', 'subscribe') */
  action: string;
  /** Additional parameters */
  params?: Record<string, unknown>;
}

/**
 * Session request parameters.
 */
export interface SessionRequest {
  /** Type of session (one-shot, streaming, persistent) */
  type: 'one-shot' | 'streaming' | 'persistent';
  /** Session time-to-live in seconds */
  ttl: number;
  /** Expected number of messages (optional) */
  expectedMessages?: number;
  /** Intent for the session */
  intent: Intent;
  /** Priority level (0-10, higher = more important) */
  priority?: number;
}

/**
 * Session state enumeration.
 */
export enum SessionStateType {
  PENDING = 'PENDING',
  ACTIVE = 'ACTIVE',
  CLOSED = 'CLOSED',
  REJECTED = 'REJECTED',
  EXPIRED = 'EXPIRED',
}

/**
 * KNOCK message for session initiation.
 */
export interface KnockMessage {
  /** Protocol version */
  version: string;
  /** Sender's AMID */
  from: string;
  /** Receiver's AMID */
  to: string;
  /** Session request details */
  request: SessionRequest;
  /** Timestamp of the message */
  timestamp: number;
  /** Signature of the message */
  signature: string;
  /** Optional certificate chain for verification */
  certificateChain?: string[];
  /** Nonce for replay prevention */
  nonce: string;
}

/**
 * KNOCK response message.
 */
export interface KnockResponse {
  /** Response type */
  type: 'ACCEPT' | 'REJECT';
  /** Session ID (for ACCEPT) */
  sessionId?: string;
  /** Rejection reason (for REJECT) */
  reason?: string;
  /** Timestamp */
  timestamp: number;
  /** Signature */
  signature: string;
  /** Responder's AMID */
  from: string;
  /** Original sender's AMID */
  to: string;
  /** Reference to original KNOCK nonce */
  knockNonce: string;
}

/**
 * Active session state.
 */
export interface SessionState {
  /** Unique session ID */
  id: string;
  /** Remote agent's AMID */
  remoteAmid: string;
  /** Current state */
  state: SessionStateType;
  /** Original request */
  request: SessionRequest;
  /** Session creation time */
  createdAt: Date;
  /** Session expiration time */
  expiresAt: Date;
  /** Messages sent in this session */
  messagesSent: number;
  /** Messages received in this session */
  messagesReceived: number;
  /** Last activity timestamp */
  lastActivity: Date;
  /** Are we the initiator? */
  isInitiator: boolean;
}

/**
 * Generate a random nonce.
 */
function generateNonce(): string {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  return Array.from(bytes)
    .map(b => b.toString(16).padStart(2, '0'))
    .join('');
}

/**
 * Generate a session ID.
 */
function generateSessionId(): string {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  return 'sess_' + Array.from(bytes)
    .map(b => b.toString(16).padStart(2, '0'))
    .join('');
}

/**
 * KNOCK protocol handler.
 */
export class KnockProtocol {
  private identity: Identity;
  private policy?: Policy;
  private certManager?: CertificateManager;
  private seenNonces: Set<string> = new Set();
  private nonceExpiryMs: number = 5 * 60 * 1000; // 5 minutes

  constructor(
    identity: Identity,
    options?: {
      policy?: Policy;
      certManager?: CertificateManager;
      nonceExpiryMs?: number;
    }
  ) {
    this.identity = identity;
    this.policy = options?.policy;
    this.certManager = options?.certManager;
    if (options?.nonceExpiryMs) {
      this.nonceExpiryMs = options.nonceExpiryMs;
    }
  }

  /**
   * Set the policy for KNOCK evaluation.
   */
  setPolicy(policy: Policy): void {
    this.policy = policy;
  }

  /**
   * Create a KNOCK message for session initiation.
   */
  async createKnock(
    toAmid: string,
    request: SessionRequest,
    certificateChain?: Certificate[]
  ): Promise<KnockMessage> {
    const now = Date.now();
    const nonce = generateNonce();

    // Create message to sign
    const messageData = {
      version: 'agentmesh/0.2',
      from: this.identity.amid,
      to: toAmid,
      request,
      timestamp: now,
      nonce,
    };

    const _messageBytes = new TextEncoder().encode(JSON.stringify(messageData));
    const signature = await this.identity.sign(messageBytes);

    const knock: KnockMessage = {
      ...messageData,
      signature: this.base64Encode(signature),
    };

    if (certificateChain && certificateChain.length > 0) {
      knock.certificateChain = certificateChain.map(c =>
        this.base64Encode(c.raw)
      );
    }

    return knock;
  }

  /**
   * Validate and process an incoming KNOCK message.
   */
  async validateKnock(knock: KnockMessage): Promise<{
    valid: boolean;
    error?: string;
    senderPublicKey?: Uint8Array;
  }> {
    // Check protocol version
    if (!knock.version.startsWith('agentmesh/')) {
      return { valid: false, error: 'Unknown protocol version' };
    }

    // Check recipient
    if (knock.to !== this.identity.amid) {
      return { valid: false, error: 'Message not addressed to us' };
    }

    // Check timestamp freshness (5 minute window)
    const now = Date.now();
    const maxAge = this.nonceExpiryMs;
    if (Math.abs(now - knock.timestamp) > maxAge) {
      return { valid: false, error: 'Message timestamp too old or in future' };
    }

    // Check for replay attack
    if (this.seenNonces.has(knock.nonce)) {
      return { valid: false, error: 'Replay attack detected: duplicate nonce' };
    }

    // Verify signature (placeholder — real implementation uses registry lookup)
    const _signatureBytes = this.base64Decode(knock.signature);
    const messageData = {
      version: knock.version,
      from: knock.from,
      to: knock.to,
      request: knock.request,
      timestamp: knock.timestamp,
      nonce: knock.nonce,
    };
    const _messageBytes = new TextEncoder().encode(JSON.stringify(messageData));

    // For now, we'll need the sender's public key from the registry
    // This is a placeholder - real implementation gets key from registry lookup
    // Return valid for now assuming signature check happens at a higher level

    // Mark nonce as seen
    this.seenNonces.add(knock.nonce);

    // Cleanup old nonces periodically
    setTimeout(() => {
      this.seenNonces.delete(knock.nonce);
    }, this.nonceExpiryMs);

    return { valid: true };
  }

  /**
   * Evaluate a KNOCK request against the policy.
   */
  async evaluateKnock(
    knock: KnockMessage,
    senderInfo?: {
      publicKey: Uint8Array;
      verificationStatus?: string;
      tier?: string;
      reputation?: number;
    }
  ): Promise<PolicyResult> {
    if (!this.policy) {
      // Default: accept all
      return { allowed: true };
    }

    const context: KnockContext = {
      fromAmid: knock.from,
      fromTier: senderInfo?.tier || 'anonymous',
      fromReputation: senderInfo?.reputation ?? 0,
      intentCategory: knock.request.intent.capability,
      requestedTtl: knock.request.ttl || 300,
    };

    return this.policy.evaluate(context);
  }

  /**
   * Create an ACCEPT response.
   */
  async createAcceptResponse(
    knock: KnockMessage,
    sessionId?: string
  ): Promise<KnockResponse> {
    const id = sessionId || generateSessionId();
    const now = Date.now();

    const responseData = {
      type: 'ACCEPT' as const,
      sessionId: id,
      timestamp: now,
      from: this.identity.amid,
      to: knock.from,
      knockNonce: knock.nonce,
    };

    const _messageBytes = new TextEncoder().encode(JSON.stringify(responseData));
    const signature = await this.identity.sign(messageBytes);

    return {
      ...responseData,
      signature: this.base64Encode(signature),
    };
  }

  /**
   * Create a REJECT response.
   */
  async createRejectResponse(
    knock: KnockMessage,
    reason: string
  ): Promise<KnockResponse> {
    const now = Date.now();

    const responseData = {
      type: 'REJECT' as const,
      reason,
      timestamp: now,
      from: this.identity.amid,
      to: knock.from,
      knockNonce: knock.nonce,
    };

    const _messageBytes = new TextEncoder().encode(JSON.stringify(responseData));
    const signature = await this.identity.sign(messageBytes);

    return {
      ...responseData,
      signature: this.base64Encode(signature),
    };
  }

  /**
   * Validate a KNOCK response.
   */
  async validateResponse(
    response: KnockResponse,
    originalKnock: KnockMessage
  ): Promise<{ valid: boolean; error?: string }> {
    // Check knockNonce matches
    if (response.knockNonce !== originalKnock.nonce) {
      return { valid: false, error: 'Response nonce does not match KNOCK' };
    }

    // Check from/to match
    if (response.from !== originalKnock.to || response.to !== originalKnock.from) {
      return { valid: false, error: 'Response addresses do not match KNOCK' };
    }

    // Check timestamp
    const now = Date.now();
    if (Math.abs(now - response.timestamp) > this.nonceExpiryMs) {
      return { valid: false, error: 'Response timestamp too old' };
    }

    return { valid: true };
  }

  /**
   * Base64 encode bytes.
   */
  private base64Encode(bytes: Uint8Array): string {
    const binary = String.fromCharCode(...bytes);
    return btoa(binary);
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

/**
 * Session manager for tracking active sessions.
 */
export class ProtocolSessionManager {
  private sessions: Map<string, SessionState> = new Map();
  private sessionsByPeer: Map<string, Set<string>> = new Map();

  /**
   * Create a new session.
   */
  createSession(
    remoteAmid: string,
    request: SessionRequest,
    isInitiator: boolean,
    sessionId?: string
  ): SessionState {
    const id = sessionId || generateSessionId();
    const now = new Date();
    const expiresAt = new Date(now.getTime() + request.ttl * 1000);

    const session: SessionState = {
      id,
      remoteAmid,
      state: SessionStateType.ACTIVE,
      request,
      createdAt: now,
      expiresAt,
      messagesSent: 0,
      messagesReceived: 0,
      lastActivity: now,
      isInitiator,
    };

    this.sessions.set(id, session);

    // Track by peer
    if (!this.sessionsByPeer.has(remoteAmid)) {
      this.sessionsByPeer.set(remoteAmid, new Set());
    }
    this.sessionsByPeer.get(remoteAmid)!.add(id);

    return session;
  }

  /**
   * Get a session by ID.
   */
  getSession(sessionId: string): SessionState | undefined {
    return this.sessions.get(sessionId);
  }

  /**
   * Get sessions for a peer.
   */
  getSessionsForPeer(remoteAmid: string): SessionState[] {
    const sessionIds = this.sessionsByPeer.get(remoteAmid);
    if (!sessionIds) return [];

    return Array.from(sessionIds)
      .map(id => this.sessions.get(id))
      .filter((s): s is SessionState => s !== undefined);
  }

  /**
   * Get all active sessions.
   */
  getActiveSessions(): SessionState[] {
    return Array.from(this.sessions.values())
      .filter(s => s.state === SessionStateType.ACTIVE);
  }

  /**
   * Update session state.
   */
  updateSessionState(sessionId: string, state: SessionStateType): void {
    const session = this.sessions.get(sessionId);
    if (session) {
      session.state = state;
      session.lastActivity = new Date();
    }
  }

  /**
   * Record a sent message.
   */
  recordMessageSent(sessionId: string): void {
    const session = this.sessions.get(sessionId);
    if (session) {
      session.messagesSent++;
      session.lastActivity = new Date();

      // Check message limit
      if (session.request.expectedMessages &&
          session.messagesSent >= session.request.expectedMessages) {
        // May need to close for one-shot sessions
        if (session.request.type === 'one-shot') {
          session.state = SessionStateType.CLOSED;
        }
      }
    }
  }

  /**
   * Record a received message.
   */
  recordMessageReceived(sessionId: string): void {
    const session = this.sessions.get(sessionId);
    if (session) {
      session.messagesReceived++;
      session.lastActivity = new Date();
    }
  }

  /**
   * Close a session.
   */
  closeSession(sessionId: string): void {
    const session = this.sessions.get(sessionId);
    if (session) {
      session.state = SessionStateType.CLOSED;

      // Remove from peer tracking
      const peerSessions = this.sessionsByPeer.get(session.remoteAmid);
      if (peerSessions) {
        peerSessions.delete(sessionId);
        if (peerSessions.size === 0) {
          this.sessionsByPeer.delete(session.remoteAmid);
        }
      }
    }
  }

  /**
   * Clean up expired sessions.
   */
  cleanupExpiredSessions(): SessionState[] {
    const now = new Date();
    const expired: SessionState[] = [];

    for (const session of this.sessions.values()) {
      if (session.expiresAt < now && session.state === SessionStateType.ACTIVE) {
        session.state = SessionStateType.EXPIRED;
        expired.push(session);

        // Remove from peer tracking
        const peerSessions = this.sessionsByPeer.get(session.remoteAmid);
        if (peerSessions) {
          peerSessions.delete(session.id);
          if (peerSessions.size === 0) {
            this.sessionsByPeer.delete(session.remoteAmid);
          }
        }
      }
    }

    return expired;
  }

  /**
   * Get session statistics.
   */
  getStats(): {
    total: number;
    active: number;
    closed: number;
    expired: number;
    rejected: number;
  } {
    let active = 0, closed = 0, expired = 0, rejected = 0;

    for (const session of this.sessions.values()) {
      switch (session.state) {
        case SessionStateType.ACTIVE:
          active++;
          break;
        case SessionStateType.CLOSED:
          closed++;
          break;
        case SessionStateType.EXPIRED:
          expired++;
          break;
        case SessionStateType.REJECTED:
          rejected++;
          break;
      }
    }

    return {
      total: this.sessions.size,
      active,
      closed,
      expired,
      rejected,
    };
  }

  /**
   * Clear all sessions.
   */
  clear(): void {
    this.sessions.clear();
    this.sessionsByPeer.clear();
  }
}

/**
 * Serialize an Intent for wire transport.
 */
export function serializeIntentToJSON(intent: Intent): Record<string, unknown> {
  return {
    capability: intent.capability,
    action: intent.action,
    params: intent.params,
  };
}

/**
 * Deserialize an Intent from wire transport.
 */
export function deserializeIntentFromJSON(data: Record<string, unknown>): Intent {
  return {
    capability: data.capability as string,
    action: data.action as string,
    params: data.params as Record<string, unknown> | undefined,
  };
}

// Re-export session cache
export { SessionCache } from './cache';
export type { CachedSession, CacheStats, SessionCacheConfig } from './cache';
