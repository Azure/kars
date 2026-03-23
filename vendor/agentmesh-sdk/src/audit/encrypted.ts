/**
 * Encrypted Audit Logger for transcript encryption.
 * Uses AES-GCM with HKDF-derived keys.
 */

import { AuditLogger, AuditLoggerConfig, AuditEvent, AuditEventType, AuditSeverity, AuditQueryOptions } from './index';
import { hkdfSimple } from '../encryption/hkdf';
import { Identity } from '../identity';

/**
 * Encrypted audit entry structure.
 */
export interface EncryptedAuditEntry {
  /** Nonce (IV) as base64 */
  nonce: string;
  /** Encrypted ciphertext as base64 */
  ciphertext: string;
  /** Entry ID (not encrypted, for indexing) */
  id: string;
  /** Timestamp (not encrypted, for indexing) */
  timestamp: string;
}

/**
 * Encrypted audit logger configuration.
 */
export interface EncryptedAuditLoggerConfig extends AuditLoggerConfig {
  /** Enable encryption (default: true) */
  encrypted?: boolean;
}

/**
 * Serialized audit event for storage.
 */
interface AuditEventSerialized {
  id: string;
  type: string;
  severity: string;
  timestamp: string;
  amid: string;
  peerAmid?: string;
  sessionId?: string;
  message: string;
  metadata?: Record<string, unknown>;
  error?: {
    name: string;
    message: string;
    stack?: string;
  };
}

/**
 * Convert Uint8Array to ArrayBuffer.
 */
function toArrayBuffer(data: Uint8Array): ArrayBuffer {
  const buffer = new ArrayBuffer(data.length);
  new Uint8Array(buffer).set(data);
  return buffer;
}

/**
 * Base64 encode.
 */
function toBase64(data: Uint8Array): string {
  let binary = '';
  for (let i = 0; i < data.length; i++) {
    binary += String.fromCharCode(data[i]!);
  }
  return btoa(binary);
}

/**
 * Base64 decode.
 */
function fromBase64(b64: string): Uint8Array {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

/**
 * Encrypted Audit Logger.
 * Wraps base AuditLogger with encryption capabilities.
 */
export class EncryptedAuditLogger {
  private readonly baseLogger: AuditLogger;
  private encryptionKey: Uint8Array | null = null;
  private readonly encrypted: boolean;
  private encryptedEntries: EncryptedAuditEntry[] = [];
  private keyInitialized: boolean = false;

  constructor(config: EncryptedAuditLoggerConfig) {
    this.baseLogger = new AuditLogger(config);
    this.encrypted = config.encrypted ?? true;

    if (!this.encrypted) {
      console.warn('Audit encryption disabled. Logs will be stored in plaintext.');
    }
  }

  /**
   * Initialize encryption key from identity.
   * Must be called before logging if encryption is enabled.
   */
  async initializeKey(identity: Identity): Promise<void> {
    if (!this.encrypted) {
      this.keyInitialized = true;
      return;
    }

    // Derive encryption key from identity using HKDF
    const seed = await identity.deriveSecret('agentmesh_audit_key');
    this.encryptionKey = await hkdfSimple(seed, 'audit_encryption_key', 32);
    this.keyInitialized = true;
  }

  /**
   * Check if key is initialized.
   */
  get isKeyInitialized(): boolean {
    return this.keyInitialized;
  }

  /**
   * Log an audit event.
   */
  async log(
    type: AuditEventType,
    severity: AuditSeverity,
    message: string,
    options?: {
      peerAmid?: string;
      sessionId?: string;
      metadata?: Record<string, unknown>;
      error?: Error;
    }
  ): Promise<AuditEvent> {
    return this.baseLogger.log(type, severity, message, options);
  }

  /**
   * Query audit events.
   */
  query(options?: AuditQueryOptions): AuditEvent[] {
    return this.baseLogger.query(options);
  }

  /**
   * Get event count.
   */
  getCount(): number {
    return this.baseLogger.getCount();
  }

  /**
   * Get recent events.
   */
  getRecent(count?: number): AuditEvent[] {
    return this.baseLogger.getRecent(count);
  }

  /**
   * Clear all events.
   */
  clear(): void {
    this.baseLogger.clear();
    this.encryptedEntries = [];
  }

  /**
   * Export plaintext audit log.
   */
  export(): string {
    return this.baseLogger.export();
  }

  /**
   * Serialize event for encryption.
   */
  private serializeEvent(event: AuditEvent): AuditEventSerialized {
    return {
      id: event.id,
      type: event.type,
      severity: event.severity,
      timestamp: event.timestamp.toISOString(),
      amid: event.amid,
      peerAmid: event.peerAmid,
      sessionId: event.sessionId,
      message: event.message,
      metadata: event.metadata,
      error: event.error,
    };
  }

  /**
   * Deserialize event from decrypted data.
   */
  private deserializeEvent(data: AuditEventSerialized): AuditEvent {
    return {
      id: data.id,
      type: data.type as AuditEventType,
      severity: data.severity as AuditSeverity,
      timestamp: new Date(data.timestamp),
      amid: data.amid,
      peerAmid: data.peerAmid,
      sessionId: data.sessionId,
      message: data.message,
      metadata: data.metadata,
      error: data.error,
    };
  }

  /**
   * Encrypt an audit entry.
   */
  async encryptEntry(event: AuditEvent): Promise<EncryptedAuditEntry> {
    if (!this.encrypted || !this.encryptionKey) {
      throw new Error('Encryption not initialized');
    }

    const plaintext = new TextEncoder().encode(JSON.stringify(this.serializeEvent(event)));

    // Import key for AES-GCM
    const aesKey = await crypto.subtle.importKey(
      'raw',
      toArrayBuffer(this.encryptionKey),
      { name: 'AES-GCM' },
      false,
      ['encrypt']
    );

    // Generate random nonce
    const nonce = crypto.getRandomValues(new Uint8Array(12));

    // Encrypt
    const ciphertext = await crypto.subtle.encrypt(
      { name: 'AES-GCM', iv: toArrayBuffer(nonce) },
      aesKey,
      toArrayBuffer(plaintext)
    );

    return {
      nonce: toBase64(nonce),
      ciphertext: toBase64(new Uint8Array(ciphertext)),
      id: event.id,
      timestamp: event.timestamp.toISOString(),
    };
  }

  /**
   * Decrypt an audit entry.
   */
  async decryptEntry(entry: EncryptedAuditEntry): Promise<AuditEvent> {
    if (!this.encrypted || !this.encryptionKey) {
      throw new Error('Encryption not initialized');
    }

    const nonce = fromBase64(entry.nonce);
    const ciphertext = fromBase64(entry.ciphertext);

    // Import key for AES-GCM
    const aesKey = await crypto.subtle.importKey(
      'raw',
      toArrayBuffer(this.encryptionKey),
      { name: 'AES-GCM' },
      false,
      ['decrypt']
    );

    // Decrypt
    const plaintext = await crypto.subtle.decrypt(
      { name: 'AES-GCM', iv: toArrayBuffer(nonce) },
      aesKey,
      toArrayBuffer(ciphertext)
    );

    const data = JSON.parse(new TextDecoder().decode(plaintext)) as AuditEventSerialized;
    return this.deserializeEvent(data);
  }

  /**
   * Export encrypted audit log.
   * Returns entries filtered by options, encrypted if encryption is enabled.
   */
  async exportAuditLog(
    identity: Identity,
    options?: AuditQueryOptions
  ): Promise<{ entries: EncryptedAuditEntry[]; encrypted: boolean }> {
    if (!this.keyInitialized) {
      await this.initializeKey(identity);
    }

    const events = this.query(options);

    if (!this.encrypted) {
      // Return plaintext entries with fake encryption format
      const entries = events.map(event => ({
        nonce: '',
        ciphertext: toBase64(new TextEncoder().encode(JSON.stringify(this.serializeEvent(event)))),
        id: event.id,
        timestamp: event.timestamp.toISOString(),
      }));
      return { entries, encrypted: false };
    }

    const entries: EncryptedAuditEntry[] = [];
    for (const event of events) {
      entries.push(await this.encryptEntry(event));
    }

    return { entries, encrypted: true };
  }

  /**
   * Import and decrypt audit log.
   */
  async importAuditLog(
    identity: Identity,
    data: { entries: EncryptedAuditEntry[]; encrypted: boolean }
  ): Promise<number> {
    if (!this.keyInitialized) {
      await this.initializeKey(identity);
    }

    let imported = 0;

    for (const entry of data.entries) {
      try {
        let event: AuditEvent;

        if (data.encrypted && this.encrypted) {
          event = await this.decryptEntry(entry);
        } else {
          // Plaintext format
          const plaintext = fromBase64(entry.ciphertext);
          const eventData = JSON.parse(new TextDecoder().decode(plaintext)) as AuditEventSerialized;
          event = this.deserializeEvent(eventData);
        }

        // Import into base logger
        const json = JSON.stringify([this.serializeEvent(event)]);
        this.baseLogger.import(json);
        imported++;
      } catch {
        // Skip invalid entries
      }
    }

    return imported;
  }

  /**
   * Re-encrypt transcripts with a new key.
   * Used for key rotation.
   */
  async reencryptTranscripts(
    oldIdentity: Identity,
    newIdentity: Identity,
    onProgress?: (current: number, total: number) => void
  ): Promise<{ reencrypted: number; failed: number }> {
    if (!this.encrypted) {
      return { reencrypted: 0, failed: 0 };
    }

    // Initialize old key
    const oldSeed = await oldIdentity.deriveSecret('agentmesh_audit_key');
    const oldKey = await hkdfSimple(oldSeed, 'audit_encryption_key', 32);

    // Initialize new key
    const newSeed = await newIdentity.deriveSecret('agentmesh_audit_key');
    const newKey = await hkdfSimple(newSeed, 'audit_encryption_key', 32);

    const entries = [...this.encryptedEntries];
    let reencrypted = 0;
    let failed = 0;

    for (let i = 0; i < entries.length; i++) {
      const entry = entries[i];
      if (!entry) continue;

      try {
        // Decrypt with old key
        const nonce = fromBase64(entry.nonce);
        const ciphertext = fromBase64(entry.ciphertext);

        const oldAesKey = await crypto.subtle.importKey(
          'raw',
          toArrayBuffer(oldKey),
          { name: 'AES-GCM' },
          false,
          ['decrypt']
        );

        const plaintext = await crypto.subtle.decrypt(
          { name: 'AES-GCM', iv: toArrayBuffer(nonce) },
          oldAesKey,
          toArrayBuffer(ciphertext)
        );

        // Re-encrypt with new key
        const newAesKey = await crypto.subtle.importKey(
          'raw',
          toArrayBuffer(newKey),
          { name: 'AES-GCM' },
          false,
          ['encrypt']
        );

        const newNonce = crypto.getRandomValues(new Uint8Array(12));
        const newCiphertext = await crypto.subtle.encrypt(
          { name: 'AES-GCM', iv: toArrayBuffer(newNonce) },
          newAesKey,
          plaintext
        );

        // Update entry
        entries[i] = {
          id: entry.id,
          timestamp: entry.timestamp,
          nonce: toBase64(newNonce),
          ciphertext: toBase64(new Uint8Array(newCiphertext)),
        };

        reencrypted++;

        if (onProgress) {
          onProgress(i + 1, entries.length);
        }
      } catch {
        failed++;
      }
    }

    // Update stored entries
    this.encryptedEntries = entries;

    // Update current key
    this.encryptionKey = newKey;

    return { reencrypted, failed };
  }

  // Convenience methods delegated to base logger

  async logIdentityCreated(metadata?: Record<string, unknown>): Promise<AuditEvent> {
    return this.baseLogger.logIdentityCreated(metadata);
  }

  async logSessionInitiated(peerAmid: string, sessionId: string): Promise<AuditEvent> {
    return this.baseLogger.logSessionInitiated(peerAmid, sessionId);
  }

  async logMessageSent(peerAmid: string, sessionId: string): Promise<AuditEvent> {
    return this.baseLogger.logMessageSent(peerAmid, sessionId);
  }

  async logMessageReceived(peerAmid: string, sessionId: string): Promise<AuditEvent> {
    return this.baseLogger.logMessageReceived(peerAmid, sessionId);
  }

  async logError(message: string, error: Error, metadata?: Record<string, unknown>): Promise<AuditEvent> {
    return this.baseLogger.logError(message, error, metadata);
  }

  async logWarning(message: string, metadata?: Record<string, unknown>): Promise<AuditEvent> {
    return this.baseLogger.logWarning(message, metadata);
  }
}

/**
 * Create an encrypted audit logger.
 */
export function createEncryptedAuditLogger(
  amid: string,
  options?: Partial<EncryptedAuditLoggerConfig>
): EncryptedAuditLogger {
  return new EncryptedAuditLogger({
    amid,
    ...options,
  });
}
