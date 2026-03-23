/**
 * Session Cache for AgentMesh.
 * Provides LRU caching with TTL for encrypted sessions.
 */

import { createHash } from 'crypto';

/**
 * Cached session entry.
 */
export interface CachedSession {
  /** Session ID */
  sessionId: string;
  /** Initiator AMID */
  initiatorAmid: string;
  /** Receiver AMID */
  receiverAmid: string;
  /** Intent hash */
  intentHash: string;
  /** Creation timestamp */
  createdAt: number;
  /** Last used timestamp */
  lastUsedAt: number;
  /** Expiration timestamp */
  expiresAt: number;
  /** Usage count */
  usageCount: number;
}

/**
 * Cache statistics.
 */
export interface CacheStats {
  /** Number of cache hits */
  hits: number;
  /** Number of cache misses */
  misses: number;
  /** Number of evictions */
  evictions: number;
  /** Current cache size */
  size: number;
  /** Maximum cache size */
  maxSize: number;
}

/**
 * Session cache configuration.
 */
export interface SessionCacheConfig {
  /** Maximum number of cached sessions */
  maxCachedSessions?: number;
  /** Default TTL in milliseconds */
  defaultTtlMs?: number;
  /** Sliding window extension on use (ms) */
  slidingWindowMs?: number;
}

type CacheEventHandler = (event: { type: string; data: unknown }) => void;

/**
 * LRU Session Cache with TTL support.
 */
export class SessionCache {
  private cache: Map<string, CachedSession> = new Map();
  private readonly maxSize: number;
  private readonly defaultTtlMs: number;
  private readonly slidingWindowMs: number;

  private stats: CacheStats = {
    hits: 0,
    misses: 0,
    evictions: 0,
    size: 0,
    maxSize: 0,
  };

  private eventHandlers: CacheEventHandler[] = [];

  constructor(config: SessionCacheConfig = {}) {
    this.maxSize = config.maxCachedSessions ?? 1000;
    this.defaultTtlMs = config.defaultTtlMs ?? 3600000; // 1 hour
    this.slidingWindowMs = config.slidingWindowMs ?? 300000; // 5 minutes
    this.stats.maxSize = this.maxSize;
  }

  /**
   * Generate a cache key from initiator, receiver, and intent.
   */
  private generateKey(initiatorAmid: string, receiverAmid: string, intent: string): string {
    const intentHash = this.hashIntent(intent);
    return `${initiatorAmid}:${receiverAmid}:${intentHash}`;
  }

  /**
   * Hash an intent string to a fixed-size key.
   */
  private hashIntent(intent: string): string {
    // Use a simple hash for browser compatibility
    if (typeof window !== 'undefined') {
      // Browser: use simple string hash
      let hash = 0;
      for (let i = 0; i < intent.length; i++) {
        const char = intent.charCodeAt(i);
        hash = ((hash << 5) - hash) + char;
        hash = hash & hash; // Convert to 32bit integer
      }
      return Math.abs(hash).toString(16).padStart(8, '0');
    } else {
      // Node.js: use crypto
      return createHash('sha256').update(intent).digest('hex').substring(0, 16);
    }
  }

  /**
   * Get a cached session if it exists and hasn't expired.
   */
  get(initiatorAmid: string, receiverAmid: string, intent: string): CachedSession | null {
    const key = this.generateKey(initiatorAmid, receiverAmid, intent);
    const entry = this.cache.get(key);

    if (!entry) {
      this.stats.misses++;
      return null;
    }

    // Check expiration
    const now = Date.now();
    if (now > entry.expiresAt) {
      this.cache.delete(key);
      this.stats.size = this.cache.size;
      this.stats.misses++;
      return null;
    }

    // Update LRU order by removing and re-adding
    this.cache.delete(key);
    entry.lastUsedAt = now;
    entry.usageCount++;

    // Extend TTL with sliding window
    entry.expiresAt = Math.min(
      entry.expiresAt + this.slidingWindowMs,
      entry.createdAt + this.defaultTtlMs * 2 // Max extension
    );

    this.cache.set(key, entry);
    this.stats.hits++;

    return entry;
  }

  /**
   * Check if a session is cached without updating LRU order.
   */
  has(initiatorAmid: string, receiverAmid: string, intent: string): boolean {
    const key = this.generateKey(initiatorAmid, receiverAmid, intent);
    const entry = this.cache.get(key);

    if (!entry) return false;

    // Check expiration
    if (Date.now() > entry.expiresAt) {
      this.cache.delete(key);
      this.stats.size = this.cache.size;
      return false;
    }

    return true;
  }

  /**
   * Cache a session.
   */
  set(
    sessionId: string,
    initiatorAmid: string,
    receiverAmid: string,
    intent: string,
    ttlMs?: number
  ): void {
    const key = this.generateKey(initiatorAmid, receiverAmid, intent);
    const now = Date.now();
    const ttl = ttlMs ?? this.defaultTtlMs;

    // Evict if at capacity
    if (this.cache.size >= this.maxSize && !this.cache.has(key)) {
      this.evictLRU();
    }

    const entry: CachedSession = {
      sessionId,
      initiatorAmid,
      receiverAmid,
      intentHash: this.hashIntent(intent),
      createdAt: now,
      lastUsedAt: now,
      expiresAt: now + ttl,
      usageCount: 0,
    };

    this.cache.set(key, entry);
    this.stats.size = this.cache.size;
  }

  /**
   * Evict the least recently used entry.
   */
  private evictLRU(): void {
    // Map maintains insertion order, so first entry is LRU
    const firstKey = this.cache.keys().next().value;
    if (firstKey) {
      const evicted = this.cache.get(firstKey);
      this.cache.delete(firstKey);
      this.stats.evictions++;
      this.stats.size = this.cache.size;
      this.emitEvent('eviction', { key: firstKey, session: evicted });
    }
  }

  /**
   * Clear a specific cached session.
   */
  clear(initiatorAmid: string, receiverAmid: string, intent: string): boolean {
    const key = this.generateKey(initiatorAmid, receiverAmid, intent);
    const existed = this.cache.delete(key);
    this.stats.size = this.cache.size;
    return existed;
  }

  /**
   * Clear a session by AMID (clears all sessions with that peer).
   */
  clearByAmid(amid: string): number {
    let cleared = 0;
    for (const [key, entry] of this.cache.entries()) {
      if (entry.initiatorAmid === amid || entry.receiverAmid === amid) {
        this.cache.delete(key);
        cleared++;
      }
    }
    this.stats.size = this.cache.size;
    return cleared;
  }

  /**
   * Clear all cached sessions.
   */
  clearAll(): void {
    const previousSize = this.cache.size;
    this.cache.clear();
    this.stats.size = 0;
    this.emitEvent('cache_cleared', { clearedCount: previousSize });
  }

  /**
   * Get cache statistics.
   */
  getStats(): CacheStats {
    return { ...this.stats };
  }

  /**
   * Get all cached sessions (for debugging/dashboard).
   */
  getAll(): CachedSession[] {
    const now = Date.now();
    const sessions: CachedSession[] = [];

    for (const entry of this.cache.values()) {
      if (now <= entry.expiresAt) {
        sessions.push({ ...entry });
      }
    }

    return sessions;
  }

  /**
   * Clean up expired sessions.
   */
  cleanup(): number {
    const now = Date.now();
    let cleaned = 0;

    for (const [key, entry] of this.cache.entries()) {
      if (now > entry.expiresAt) {
        this.cache.delete(key);
        cleaned++;
      }
    }

    this.stats.size = this.cache.size;
    return cleaned;
  }

  /**
   * Register an event handler.
   */
  onEvent(handler: CacheEventHandler): void {
    this.eventHandlers.push(handler);
  }

  /**
   * Emit an event to all handlers.
   */
  private emitEvent(type: string, data: unknown): void {
    for (const handler of this.eventHandlers) {
      try {
        handler({ type, data });
      } catch {
        // Ignore handler errors
      }
    }
  }
}
