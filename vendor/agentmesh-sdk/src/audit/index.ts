/**
 * Audit module for AgentMesh.
 * Provides audit logging and event tracking.
 */

import type { Storage } from '../storage';

/**
 * Audit event types.
 */
export type AuditEventType =
  | 'IDENTITY_CREATED'
  | 'IDENTITY_LOADED'
  | 'IDENTITY_ROTATED'
  | 'SESSION_INITIATED'
  | 'SESSION_ACCEPTED'
  | 'SESSION_REJECTED'
  | 'SESSION_CLOSED'
  | 'SESSION_CACHED'
  | 'CACHE_CLEARED'
  | 'MESSAGE_SENT'
  | 'MESSAGE_RECEIVED'
  | 'MESSAGE_DECRYPTION_FAILED'
  | 'KNOCK_SENT'
  | 'KNOCK_RECEIVED'
  | 'KNOCK_VALIDATED'
  | 'KNOCK_REJECTED'
  | 'PREKEY_GENERATED'
  | 'PREKEY_ROTATED'
  | 'PREKEY_CONSUMED'
  | 'CERTIFICATE_VERIFIED'
  | 'CERTIFICATE_INVALID'
  | 'CONNECTION_ESTABLISHED'
  | 'CONNECTION_LOST'
  | 'CONNECTION_FAILED'
  | 'REGISTRY_REGISTERED'
  | 'REGISTRY_LOOKUP'
  | 'REGISTRY_SEARCH'
  | 'POLICY_EVALUATED'
  | 'ERROR'
  | 'WARNING';

/**
 * Audit event severity levels.
 */
export type AuditSeverity = 'DEBUG' | 'INFO' | 'WARNING' | 'ERROR' | 'CRITICAL';

/**
 * Audit event structure.
 */
export interface AuditEvent {
  /** Unique event ID */
  id: string;
  /** Event type */
  type: AuditEventType;
  /** Event severity */
  severity: AuditSeverity;
  /** Timestamp */
  timestamp: Date;
  /** Our AMID */
  amid: string;
  /** Related peer AMID (if applicable) */
  peerAmid?: string;
  /** Session ID (if applicable) */
  sessionId?: string;
  /** Event message */
  message: string;
  /** Additional metadata */
  metadata?: Record<string, unknown>;
  /** Error details (if applicable) */
  error?: {
    name: string;
    message: string;
    stack?: string;
  };
}

/**
 * Serialized audit event for storage.
 */
export interface AuditEventSerialized {
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
 * Audit query options.
 */
export interface AuditQueryOptions {
  /** Filter by event type */
  type?: AuditEventType | AuditEventType[];
  /** Filter by severity */
  severity?: AuditSeverity | AuditSeverity[];
  /** Filter by peer AMID */
  peerAmid?: string;
  /** Filter by session ID */
  sessionId?: string;
  /** Start time (inclusive) */
  startTime?: Date;
  /** End time (inclusive) */
  endTime?: Date;
  /** Maximum number of results */
  limit?: number;
  /** Offset for pagination */
  offset?: number;
  /** Sort order */
  order?: 'asc' | 'desc';
}

/**
 * Audit logger configuration.
 */
export interface AuditLoggerConfig {
  /** Our AMID */
  amid: string;
  /** Storage backend (optional, for persistence) */
  storage?: Storage;
  /** Storage key prefix */
  storagePrefix?: string;
  /** Maximum events to keep in memory */
  maxMemoryEvents?: number;
  /** Minimum severity to log */
  minSeverity?: AuditSeverity;
  /** Enable console output */
  consoleOutput?: boolean;
}

/**
 * Severity priority order.
 */
const SEVERITY_PRIORITY: Record<AuditSeverity, number> = {
  DEBUG: 0,
  INFO: 1,
  WARNING: 2,
  ERROR: 3,
  CRITICAL: 4,
};

/**
 * Generate a unique event ID.
 */
function generateEventId(): string {
  const timestamp = Date.now().toString(36);
  const random = Math.random().toString(36).slice(2, 10);
  return `audit_${timestamp}_${random}`;
}

/**
 * Audit logger for tracking events.
 */
export class AuditLogger {
  private config: AuditLoggerConfig;
  private events: AuditEvent[] = [];
  private minSeverityLevel: number;

  constructor(config: AuditLoggerConfig) {
    this.config = {
      maxMemoryEvents: 1000,
      minSeverity: 'INFO',
      consoleOutput: false,
      storagePrefix: 'audit',
      ...config,
    };

    this.minSeverityLevel = SEVERITY_PRIORITY[this.config.minSeverity || 'INFO'];
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
    // Check severity threshold
    if (SEVERITY_PRIORITY[severity] < this.minSeverityLevel) {
      // Below threshold, still create event but don't store
      return this.createEvent(type, severity, message, options);
    }

    const event = this.createEvent(type, severity, message, options);

    // Add to memory store
    this.events.push(event);

    // Trim if over limit
    if (this.events.length > (this.config.maxMemoryEvents || 1000)) {
      this.events.shift();
    }

    // Persist to storage if available
    if (this.config.storage) {
      await this.persistEvent(event);
    }

    // Console output if enabled
    if (this.config.consoleOutput) {
      this.logToConsole(event);
    }

    return event;
  }

  /**
   * Create an event object.
   */
  private createEvent(
    type: AuditEventType,
    severity: AuditSeverity,
    message: string,
    options?: {
      peerAmid?: string;
      sessionId?: string;
      metadata?: Record<string, unknown>;
      error?: Error;
    }
  ): AuditEvent {
    const event: AuditEvent = {
      id: generateEventId(),
      type,
      severity,
      timestamp: new Date(),
      amid: this.config.amid,
      message,
    };

    if (options?.peerAmid) {
      event.peerAmid = options.peerAmid;
    }

    if (options?.sessionId) {
      event.sessionId = options.sessionId;
    }

    if (options?.metadata) {
      event.metadata = options.metadata;
    }

    if (options?.error) {
      event.error = {
        name: options.error.name,
        message: options.error.message,
        stack: options.error.stack,
      };
    }

    return event;
  }

  /**
   * Persist an event to storage.
   */
  private async persistEvent(event: AuditEvent): Promise<void> {
    if (!this.config.storage) return;

    const key = `${this.config.storagePrefix}/${event.id}`;
    const serialized = this.serializeEvent(event);
    await this.config.storage.set(key, new TextEncoder().encode(JSON.stringify(serialized)));
  }

  /**
   * Serialize an event for storage.
   */
  private serializeEvent(event: AuditEvent): AuditEventSerialized {
    return {
      ...event,
      timestamp: event.timestamp.toISOString(),
    };
  }

  /**
   * Deserialize an event from storage.
   */
  private deserializeEvent(data: AuditEventSerialized): AuditEvent {
    return {
      ...data,
      type: data.type as AuditEventType,
      severity: data.severity as AuditSeverity,
      timestamp: new Date(data.timestamp),
    };
  }

  /**
   * Log to console.
   */
  private logToConsole(event: AuditEvent): void {
    const prefix = `[${event.severity}] [${event.type}]`;
    const msg = `${prefix} ${event.message}`;

    switch (event.severity) {
      case 'DEBUG':
        console.debug(msg, event.metadata || '');
        break;
      case 'INFO':
        console.info(msg, event.metadata || '');
        break;
      case 'WARNING':
        console.warn(msg, event.metadata || '');
        break;
      case 'ERROR':
      case 'CRITICAL':
        console.error(msg, event.error || event.metadata || '');
        break;
    }
  }

  /**
   * Query audit events.
   */
  query(options: AuditQueryOptions = {}): AuditEvent[] {
    let results = [...this.events];

    // Filter by type
    if (options.type) {
      const types = Array.isArray(options.type) ? options.type : [options.type];
      results = results.filter(e => types.includes(e.type));
    }

    // Filter by severity
    if (options.severity) {
      const severities = Array.isArray(options.severity) ? options.severity : [options.severity];
      results = results.filter(e => severities.includes(e.severity));
    }

    // Filter by peer
    if (options.peerAmid) {
      results = results.filter(e => e.peerAmid === options.peerAmid);
    }

    // Filter by session
    if (options.sessionId) {
      results = results.filter(e => e.sessionId === options.sessionId);
    }

    // Filter by time range
    if (options.startTime) {
      results = results.filter(e => e.timestamp >= options.startTime!);
    }
    if (options.endTime) {
      results = results.filter(e => e.timestamp <= options.endTime!);
    }

    // Sort
    const order = options.order || 'desc';
    results.sort((a, b) => {
      const diff = a.timestamp.getTime() - b.timestamp.getTime();
      return order === 'asc' ? diff : -diff;
    });

    // Pagination
    const offset = options.offset || 0;
    const limit = options.limit || results.length;
    results = results.slice(offset, offset + limit);

    return results;
  }

  /**
   * Get events by type.
   */
  getByType(type: AuditEventType): AuditEvent[] {
    return this.query({ type });
  }

  /**
   * Get events for a peer.
   */
  getByPeer(peerAmid: string): AuditEvent[] {
    return this.query({ peerAmid });
  }

  /**
   * Get events for a session.
   */
  getBySession(sessionId: string): AuditEvent[] {
    return this.query({ sessionId });
  }

  /**
   * Get error events.
   */
  getErrors(): AuditEvent[] {
    return this.query({ severity: ['ERROR', 'CRITICAL'] });
  }

  /**
   * Get recent events.
   */
  getRecent(count: number = 10): AuditEvent[] {
    return this.query({ limit: count, order: 'desc' });
  }

  /**
   * Get event count.
   */
  getCount(): number {
    return this.events.length;
  }

  /**
   * Get event statistics.
   */
  getStats(): {
    total: number;
    byType: Record<string, number>;
    bySeverity: Record<string, number>;
  } {
    const byType: Record<string, number> = {};
    const bySeverity: Record<string, number> = {};

    for (const event of this.events) {
      byType[event.type] = (byType[event.type] || 0) + 1;
      bySeverity[event.severity] = (bySeverity[event.severity] || 0) + 1;
    }

    return {
      total: this.events.length,
      byType,
      bySeverity,
    };
  }

  /**
   * Clear all events from memory.
   */
  clear(): void {
    this.events = [];
  }

  /**
   * Export all events to JSON.
   */
  export(): string {
    const serialized = this.events.map(e => this.serializeEvent(e));
    return JSON.stringify(serialized, null, 2);
  }

  /**
   * Import events from JSON.
   */
  import(json: string): number {
    const data = JSON.parse(json) as AuditEventSerialized[];
    let imported = 0;

    for (const item of data) {
      try {
        const event = this.deserializeEvent(item);
        this.events.push(event);
        imported++;
      } catch {
        // Skip invalid events
      }
    }

    // Sort by timestamp
    this.events.sort((a, b) => a.timestamp.getTime() - b.timestamp.getTime());

    // Trim if over limit
    while (this.events.length > (this.config.maxMemoryEvents || 1000)) {
      this.events.shift();
    }

    return imported;
  }

  // Convenience methods for common events

  /** Log identity creation */
  async logIdentityCreated(metadata?: Record<string, unknown>): Promise<AuditEvent> {
    return this.log('IDENTITY_CREATED', 'INFO', 'Identity created', { metadata });
  }

  /** Log session initiation */
  async logSessionInitiated(peerAmid: string, sessionId: string): Promise<AuditEvent> {
    return this.log('SESSION_INITIATED', 'INFO', `Session initiated with ${peerAmid}`, {
      peerAmid,
      sessionId,
    });
  }

  /** Log message sent */
  async logMessageSent(peerAmid: string, sessionId: string): Promise<AuditEvent> {
    return this.log('MESSAGE_SENT', 'DEBUG', `Message sent to ${peerAmid}`, {
      peerAmid,
      sessionId,
    });
  }

  /** Log message received */
  async logMessageReceived(peerAmid: string, sessionId: string): Promise<AuditEvent> {
    return this.log('MESSAGE_RECEIVED', 'DEBUG', `Message received from ${peerAmid}`, {
      peerAmid,
      sessionId,
    });
  }

  /** Log error */
  async logError(message: string, error: Error, metadata?: Record<string, unknown>): Promise<AuditEvent> {
    return this.log('ERROR', 'ERROR', message, { error, metadata });
  }

  /** Log warning */
  async logWarning(message: string, metadata?: Record<string, unknown>): Promise<AuditEvent> {
    return this.log('WARNING', 'WARNING', message, { metadata });
  }
}

/**
 * Create an audit logger with default configuration.
 */
export function createAuditLogger(amid: string, options?: Partial<AuditLoggerConfig>): AuditLogger {
  return new AuditLogger({
    amid,
    ...options,
  });
}

// Re-export encrypted audit logger
export {
  EncryptedAuditLogger,
  createEncryptedAuditLogger,
} from './encrypted';
export type {
  EncryptedAuditEntry,
  EncryptedAuditLoggerConfig,
} from './encrypted';
