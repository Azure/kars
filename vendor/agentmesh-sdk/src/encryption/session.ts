/**
 * Encrypted session management.
 * Handles session lifecycle, persistence, and E2E encryption.
 */

import { Identity } from '../identity';
import { Storage } from '../storage/interface';
import { X3DHKeyExchange, X3DHInitiatorMessage, deserializeX3DHMessage } from './x3dh';
import { DoubleRatchetSession, EncryptedMessage, serializeRatchetHeader, deserializeRatchetHeader } from './ratchet';
import { PrekeyManager, PrekeyBundle, deserializePrekeyBundle } from './prekey';

/**
 * Session state enumeration.
 */
export enum SessionState {
  /** Session pending establishment */
  PENDING = 'pending',
  /** Session is active */
  ACTIVE = 'active',
  /** Session was closed normally */
  CLOSED = 'closed',
  /** Session was rejected */
  REJECTED = 'rejected',
}

/**
 * Information about an encrypted session.
 */
export interface SessionInfo {
  sessionId: string;
  peerAmid: string;
  state: SessionState;
  createdAt: Date;
  lastUsed: Date;
  isInitiator: boolean;
  messagesSent: number;
  messagesReceived: number;
}

/**
 * Serialized session for storage.
 */
interface SessionData {
  session_id: string;
  peer_amid: string;
  state: string;
  created_at: string;
  last_used: string;
  is_initiator: boolean;
  messages_sent: number;
  messages_received: number;
  ratchet_state: Record<string, unknown>;
}

/**
 * Encrypted message envelope for wire transport.
 */
export interface MessageEnvelope {
  /** Session ID */
  session_id: string;
  /** Message type */
  type: string;
  /** Ratchet header */
  header: {
    dh_public_key: string;
    pn: number;
    n: number;
  };
  /** Base64-encoded ciphertext */
  ciphertext: string;
}

/**
 * Session configuration.
 */
export interface SessionConfig {
  /** Session TTL in seconds (default: 300) */
  ttl?: number;
  /** Enable Double Ratchet (default: true) */
  useDoubleRatchet?: boolean;
  /** Cleanup interval in ms (default: 6 hours) */
  cleanupInterval?: number;
  /** Stale session threshold in ms (default: 7 days) */
  staleThreshold?: number;
}

const DEFAULT_SESSION_CONFIG: Required<SessionConfig> = {
  ttl: 300,
  useDoubleRatchet: true,
  cleanupInterval: 6 * 60 * 60 * 1000, // 6 hours
  staleThreshold: 7 * 24 * 60 * 60 * 1000, // 7 days
};

/**
 * Session manager for end-to-end encrypted messaging.
 */
export class SessionManager {
  private readonly identity: Identity;
  private readonly storage: Storage;
  private readonly config: Required<SessionConfig>;
  private readonly prekeyManager: PrekeyManager;

  private sessions: Map<string, {
    info: SessionInfo;
    ratchet: DoubleRatchetSession;
  }> = new Map();

  private cleanupTimer: ReturnType<typeof setInterval> | null = null;

  constructor(
    identity: Identity,
    storage: Storage,
    prekeyManager: PrekeyManager,
    config: SessionConfig = {}
  ) {
    this.identity = identity;
    this.storage = storage;
    this.prekeyManager = prekeyManager;
    this.config = { ...DEFAULT_SESSION_CONFIG, ...config };
  }

  /**
   * Initialize session manager, loading persisted sessions.
   */
  async initialize(): Promise<number> {
    const loaded = await this.loadAllSessions();

    // Start cleanup timer
    this.cleanupTimer = setInterval(
      () => this.cleanupStaleSessions(),
      this.config.cleanupInterval
    );

    return loaded;
  }

  /**
   * Shutdown session manager.
   */
  async shutdown(): Promise<void> {
    if (this.cleanupTimer) {
      clearInterval(this.cleanupTimer);
      this.cleanupTimer = null;
    }

    // Save all sessions
    for (const [sessionId, session] of this.sessions) {
      await this.saveSession(sessionId, session.info, session.ratchet);
    }
  }

  /**
   * Initiate a new encrypted session with a peer.
   *
   * @param peerAmid - The peer's AgentMesh ID
   * @param peerBundle - The peer's prekey bundle
   * @param peerSigningKey - The peer's signing public key
   * @returns Session ID and initial X3DH message to send
   */
  async initiateSession(
    peerAmid: string,
    peerBundle: PrekeyBundle,
    peerSigningKey: Uint8Array
  ): Promise<{ sessionId: string; x3dhMessage: X3DHInitiatorMessage }> {
    // Check for existing session
    const existing = this.getSessionByPeer(peerAmid);
    if (existing && existing.state === SessionState.ACTIVE) {
      throw new Error(`Active session already exists with ${peerAmid}`);
    }

    // Perform X3DH key exchange
    const x3dhResult = await X3DHKeyExchange.initiator(
      this.identity,
      peerBundle,
      peerSigningKey
    );

    // Generate session ID
    const sessionId = await this.generateSessionId();

    // Initialize Double Ratchet
    // The peer's signed prekey serves as the initial remote ratchet public key
    // (per Signal Protocol X3DH → Double Ratchet handoff)
    const ratchet = await DoubleRatchetSession.initialize(
      x3dhResult.sharedSecret,
      true, // is initiator
      peerBundle.signedPrekey // peer's signed prekey = initial ratchet key
    );

    const info: SessionInfo = {
      sessionId,
      peerAmid,
      state: SessionState.PENDING,
      createdAt: new Date(),
      lastUsed: new Date(),
      isInitiator: true,
      messagesSent: 0,
      messagesReceived: 0,
    };

    this.sessions.set(sessionId, { info, ratchet });
    await this.saveSession(sessionId, info, ratchet);

    return { sessionId, x3dhMessage: x3dhResult.initiatorMessage };
  }

  /**
   * Accept an incoming session from a peer.
   *
   * @param peerAmid - The peer's AgentMesh ID
   * @param x3dhMessage - The X3DH message from the initiator
   * @returns Session ID
   */
  async acceptSession(
    peerAmid: string,
    x3dhMessage: X3DHInitiatorMessage
  ): Promise<string> {
    // Get our prekeys
    const signedPrekeyPrivate = this.prekeyManager.getSignedPrekeyPrivate(x3dhMessage.signedPrekeyId);
    if (!signedPrekeyPrivate) {
      throw new Error(`Signed prekey ${x3dhMessage.signedPrekeyId} not found`);
    }

    let oneTimePrekeyPrivate: Uint8Array | null = null;
    if (x3dhMessage.oneTimePrekeyId !== undefined) {
      oneTimePrekeyPrivate = this.prekeyManager.getOneTimePrekeyPrivate(x3dhMessage.oneTimePrekeyId);
      if (!oneTimePrekeyPrivate) {
        throw new Error(`One-time prekey ${x3dhMessage.oneTimePrekeyId} not found`);
      }
      // Consume the one-time prekey
      await this.prekeyManager.consumePrekey(x3dhMessage.oneTimePrekeyId);
    }

    // Perform X3DH as responder
    const x3dhResult = await X3DHKeyExchange.responder(
      this.identity,
      signedPrekeyPrivate,
      oneTimePrekeyPrivate,
      x3dhMessage
    );

    // Generate session ID
    const sessionId = await this.generateSessionId();

    // Initialize Double Ratchet with peer's ephemeral key as initial ratchet key
    const ratchet = await DoubleRatchetSession.initialize(
      x3dhResult.sharedSecret,
      false, // not initiator
      x3dhMessage.ephemeralKey // peer's ratchet public key
    );

    const info: SessionInfo = {
      sessionId,
      peerAmid,
      state: SessionState.ACTIVE,
      createdAt: new Date(),
      lastUsed: new Date(),
      isInitiator: false,
      messagesSent: 0,
      messagesReceived: 0,
    };

    this.sessions.set(sessionId, { info, ratchet });
    await this.saveSession(sessionId, info, ratchet);

    return sessionId;
  }

  /**
   * Activate a pending session (after receiving ACK from responder).
   */
  async activateSession(sessionId: string, peerRatchetKey: Uint8Array): Promise<void> {
    const session = this.sessions.get(sessionId);
    if (!session) {
      throw new Error(`Session not found: ${sessionId}`);
    }

    if (session.info.state !== SessionState.PENDING) {
      throw new Error(`Session ${sessionId} is not pending`);
    }

    // Initialize the ratchet with peer's key
    await session.ratchet.initializeReceiving(peerRatchetKey);

    session.info.state = SessionState.ACTIVE;
    session.info.lastUsed = new Date();
    await this.saveSession(sessionId, session.info, session.ratchet);
  }

  /**
   * Activate a session directly (peer DH key already set during initialization).
   */
  async activateSessionDirect(sessionId: string): Promise<void> {
    const session = this.sessions.get(sessionId);
    if (!session) {
      throw new Error(`Session not found: ${sessionId}`);
    }

    session.info.state = SessionState.ACTIVE;
    session.info.lastUsed = new Date();
    await this.saveSession(sessionId, session.info, session.ratchet);
  }

  /**
   * Encrypt a message for a session.
   */
  async encryptMessage(sessionId: string, plaintext: Record<string, unknown>): Promise<MessageEnvelope> {
    const session = this.sessions.get(sessionId);
    if (!session) {
      throw new Error(`Session not found: ${sessionId}`);
    }

    if (session.info.state !== SessionState.ACTIVE) {
      throw new Error(`Session ${sessionId} is not active`);
    }

    // Serialize plaintext
    const plaintextBytes = new TextEncoder().encode(JSON.stringify(plaintext));

    // Encrypt with Double Ratchet
    const encrypted = await session.ratchet.encrypt(plaintextBytes);

    // Update session
    session.info.messagesSent++;
    session.info.lastUsed = new Date();
    await this.saveSession(sessionId, session.info, session.ratchet);

    // Build envelope
    const header = serializeRatchetHeader(encrypted.header);
    const ciphertext = this.bytesToBase64(encrypted.ciphertext);

    return {
      session_id: sessionId,
      type: 'encrypted',
      header,
      ciphertext,
    };
  }

  /**
   * Decrypt a message from a session.
   */
  async decryptMessage(sessionId: string, envelope: MessageEnvelope): Promise<Record<string, unknown>> {
    const session = this.sessions.get(sessionId);
    if (!session) {
      throw new Error(`Session not found: ${sessionId}`);
    }

    if (session.info.state !== SessionState.ACTIVE) {
      throw new Error(`Session ${sessionId} is not active`);
    }

    // Parse envelope
    const header = deserializeRatchetHeader(envelope.header);
    const ciphertext = this.base64ToBytes(envelope.ciphertext);

    // Decrypt with Double Ratchet
    const plaintextBytes = await session.ratchet.decrypt({ header, ciphertext });

    // Update session
    session.info.messagesReceived++;
    session.info.lastUsed = new Date();
    await this.saveSession(sessionId, session.info, session.ratchet);

    // Parse plaintext
    return JSON.parse(new TextDecoder().decode(plaintextBytes));
  }

  /**
   * Close a session.
   */
  async closeSession(sessionId: string, reason = 'normal'): Promise<void> {
    const session = this.sessions.get(sessionId);
    if (!session) return;

    session.info.state = SessionState.CLOSED;
    await this.saveSession(sessionId, session.info, session.ratchet);
  }

  /**
   * Get session info.
   */
  getSession(sessionId: string): SessionInfo | null {
    return this.sessions.get(sessionId)?.info ?? null;
  }

  /**
   * Get session info (alias for getSession).
   */
  getSessionInfo(sessionId: string): SessionInfo | null {
    return this.getSession(sessionId);
  }

  /**
   * Get the ratchet public key for a session.
   * Used by initiators to complete session activation after responder accepts.
   */
  getRatchetPublicKey(sessionId: string): Uint8Array | null {
    const session = this.sessions.get(sessionId);
    if (!session) return null;
    return session.ratchet.getRatchetPublicKey();
  }

  /**
   * Get session by peer AMID.
   */
  getSessionByPeer(peerAmid: string): SessionInfo | null {
    for (const session of this.sessions.values()) {
      if (session.info.peerAmid === peerAmid) {
        return session.info;
      }
    }
    return null;
  }

  /**
   * List all sessions.
   */
  listSessions(): SessionInfo[] {
    return Array.from(this.sessions.values()).map(s => s.info);
  }

  /**
   * Get session count by state.
   */
  getSessionStats(): { total: number; active: number; pending: number; closed: number } {
    let active = 0, pending = 0, closed = 0;
    for (const session of this.sessions.values()) {
      switch (session.info.state) {
        case SessionState.ACTIVE: active++; break;
        case SessionState.PENDING: pending++; break;
        case SessionState.CLOSED:
        case SessionState.REJECTED: closed++; break;
      }
    }
    return { total: this.sessions.size, active, pending, closed };
  }

  /**
   * Generate unique session ID.
   */
  private async generateSessionId(): Promise<string> {
    const bytes = crypto.getRandomValues(new Uint8Array(16));
    const hex = Array.from(bytes).map(b => b.toString(16).padStart(2, '0')).join('');
    return `session_${hex}`;
  }

  /**
   * Save session to storage.
   */
  private async saveSession(
    sessionId: string,
    info: SessionInfo,
    ratchet: DoubleRatchetSession
  ): Promise<void> {
    const data: SessionData = {
      session_id: info.sessionId,
      peer_amid: info.peerAmid,
      state: info.state,
      created_at: info.createdAt.toISOString(),
      last_used: info.lastUsed.toISOString(),
      is_initiator: info.isInitiator,
      messages_sent: info.messagesSent,
      messages_received: info.messagesReceived,
      ratchet_state: ratchet.serializeState() as unknown as Record<string, unknown>,
    };

    const path = `sessions/${sessionId}.json`;
    const bytes = new TextEncoder().encode(JSON.stringify(data, null, 2));
    await this.storage.set(path, bytes);
  }

  /**
   * Load session from storage.
   */
  private async loadSession(sessionId: string): Promise<boolean> {
    const path = `sessions/${sessionId}.json`;
    const bytes = await this.storage.get(path);
    if (!bytes) return false;

    try {
      const data = JSON.parse(new TextDecoder().decode(bytes)) as SessionData;

      const info: SessionInfo = {
        sessionId: data.session_id,
        peerAmid: data.peer_amid,
        state: data.state as SessionState,
        createdAt: new Date(data.created_at),
        lastUsed: new Date(data.last_used),
        isInitiator: data.is_initiator,
        messagesSent: data.messages_sent,
        messagesReceived: data.messages_received,
      };

      const ratchet = DoubleRatchetSession.deserializeState(
        data.ratchet_state as never,
        info.isInitiator
      );

      this.sessions.set(sessionId, { info, ratchet });
      return true;
    } catch {
      return false;
    }
  }

  /**
   * Load all persisted sessions.
   */
  private async loadAllSessions(): Promise<number> {
    const files = await this.storage.list('sessions/');
    let loaded = 0;

    for (const file of files) {
      if (file.endsWith('.json')) {
        const sessionId = file.replace('sessions/', '').replace('.json', '');
        if (await this.loadSession(sessionId)) {
          loaded++;
        }
      }
    }

    return loaded;
  }

  /**
   * Clean up stale sessions.
   */
  private async cleanupStaleSessions(): Promise<number> {
    const now = Date.now();
    const staleIds: string[] = [];

    for (const [sessionId, session] of this.sessions) {
      const age = now - session.info.lastUsed.getTime();
      if (age > this.config.staleThreshold && session.info.state !== SessionState.ACTIVE) {
        staleIds.push(sessionId);
      }
    }

    for (const sessionId of staleIds) {
      this.sessions.delete(sessionId);
      const path = `sessions/${sessionId}.json`;
      await this.storage.delete(path);
    }

    return staleIds.length;
  }

  /**
   * Helper: bytes to base64.
   */
  private bytesToBase64(bytes: Uint8Array): string {
    const binary = String.fromCharCode(...bytes);
    return btoa(binary);
  }

  /**
   * Helper: base64 to bytes.
   */
  private base64ToBytes(b64: string): Uint8Array {
    const binary = atob(b64);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) {
      bytes[i] = binary.charCodeAt(i);
    }
    return bytes;
  }
}
