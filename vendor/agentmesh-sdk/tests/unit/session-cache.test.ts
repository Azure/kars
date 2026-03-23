/**
 * Unit tests for SessionCache module.
 */
import { describe, test, expect, beforeEach, vi } from 'vitest';
import { SessionCache } from '../../src/session/cache';

describe('SessionCache', () => {
  let cache: SessionCache;

  beforeEach(() => {
    cache = new SessionCache({
      maxCachedSessions: 10,
      defaultTtlMs: 60000, // 1 minute
      slidingWindowMs: 10000, // 10 seconds
    });
  });

  describe('constructor', () => {
    test('should create cache with default options', () => {
      const defaultCache = new SessionCache();
      expect(defaultCache).toBeDefined();
      expect(defaultCache.getStats().maxSize).toBe(1000);
    });

    test('should create cache with custom options', () => {
      expect(cache.getStats().maxSize).toBe(10);
    });
  });

  describe('set and get', () => {
    test('should cache a session', () => {
      cache.set('session-1', 'alice', 'bob', 'weather/query');

      const result = cache.get('alice', 'bob', 'weather/query');

      expect(result).not.toBeNull();
      expect(result?.sessionId).toBe('session-1');
      expect(result?.initiatorAmid).toBe('alice');
      expect(result?.receiverAmid).toBe('bob');
    });

    test('should return null for non-existent session', () => {
      const result = cache.get('alice', 'bob', 'weather/query');

      expect(result).toBeNull();
    });

    test('should track usage count', () => {
      cache.set('session-1', 'alice', 'bob', 'weather/query');

      cache.get('alice', 'bob', 'weather/query');
      cache.get('alice', 'bob', 'weather/query');
      const result = cache.get('alice', 'bob', 'weather/query');

      expect(result?.usageCount).toBe(3);
    });

    test('should update lastUsedAt on get', async () => {
      cache.set('session-1', 'alice', 'bob', 'weather/query');

      const first = cache.get('alice', 'bob', 'weather/query');
      const firstUsed = first?.lastUsedAt ?? 0;

      // Wait a tiny bit
      await new Promise(resolve => setTimeout(resolve, 10));

      const second = cache.get('alice', 'bob', 'weather/query');

      expect(second?.lastUsedAt).toBeGreaterThanOrEqual(firstUsed);
    });
  });

  describe('has', () => {
    test('should return true for cached session', () => {
      cache.set('session-1', 'alice', 'bob', 'weather/query');

      expect(cache.has('alice', 'bob', 'weather/query')).toBe(true);
    });

    test('should return false for non-existent session', () => {
      expect(cache.has('alice', 'bob', 'weather/query')).toBe(false);
    });
  });

  describe('composite key', () => {
    test('should differentiate by intent', () => {
      cache.set('session-1', 'alice', 'bob', 'weather/query');
      cache.set('session-2', 'alice', 'bob', 'calendar/book');

      const weather = cache.get('alice', 'bob', 'weather/query');
      const calendar = cache.get('alice', 'bob', 'calendar/book');

      expect(weather?.sessionId).toBe('session-1');
      expect(calendar?.sessionId).toBe('session-2');
    });

    test('should differentiate by peer direction', () => {
      cache.set('session-1', 'alice', 'bob', 'weather/query');
      cache.set('session-2', 'bob', 'alice', 'weather/query');

      const aliceToBob = cache.get('alice', 'bob', 'weather/query');
      const bobToAlice = cache.get('bob', 'alice', 'weather/query');

      expect(aliceToBob?.sessionId).toBe('session-1');
      expect(bobToAlice?.sessionId).toBe('session-2');
    });
  });

  describe('TTL and expiration', () => {
    test('should expire sessions after TTL', () => {
      const shortCache = new SessionCache({
        maxCachedSessions: 10,
        defaultTtlMs: 100, // 100ms
      });

      shortCache.set('session-1', 'alice', 'bob', 'test');

      expect(shortCache.has('alice', 'bob', 'test')).toBe(true);

      // Wait for expiration
      return new Promise<void>((resolve) => {
        setTimeout(() => {
          expect(shortCache.has('alice', 'bob', 'test')).toBe(false);
          resolve();
        }, 150);
      });
    });

    test('should support custom TTL per session', () => {
      cache.set('session-1', 'alice', 'bob', 'test', 100); // 100ms TTL

      expect(cache.has('alice', 'bob', 'test')).toBe(true);

      return new Promise<void>((resolve) => {
        setTimeout(() => {
          expect(cache.has('alice', 'bob', 'test')).toBe(false);
          resolve();
        }, 150);
      });
    });
  });

  describe('LRU eviction', () => {
    test('should evict LRU entry when at capacity', () => {
      const smallCache = new SessionCache({
        maxCachedSessions: 3,
        defaultTtlMs: 60000,
      });

      smallCache.set('session-1', 'a', 'b', 'intent1');
      smallCache.set('session-2', 'a', 'b', 'intent2');
      smallCache.set('session-3', 'a', 'b', 'intent3');

      // This should evict session-1 (LRU)
      smallCache.set('session-4', 'a', 'b', 'intent4');

      expect(smallCache.has('a', 'b', 'intent1')).toBe(false);
      expect(smallCache.has('a', 'b', 'intent2')).toBe(true);
      expect(smallCache.has('a', 'b', 'intent3')).toBe(true);
      expect(smallCache.has('a', 'b', 'intent4')).toBe(true);
    });

    test('should update LRU order on get', () => {
      const smallCache = new SessionCache({
        maxCachedSessions: 3,
        defaultTtlMs: 60000,
      });

      smallCache.set('session-1', 'a', 'b', 'intent1');
      smallCache.set('session-2', 'a', 'b', 'intent2');
      smallCache.set('session-3', 'a', 'b', 'intent3');

      // Access session-1 to make it recently used
      smallCache.get('a', 'b', 'intent1');

      // This should evict session-2 (now LRU)
      smallCache.set('session-4', 'a', 'b', 'intent4');

      expect(smallCache.has('a', 'b', 'intent1')).toBe(true);
      expect(smallCache.has('a', 'b', 'intent2')).toBe(false);
      expect(smallCache.has('a', 'b', 'intent3')).toBe(true);
      expect(smallCache.has('a', 'b', 'intent4')).toBe(true);
    });

    test('should track eviction count', () => {
      const smallCache = new SessionCache({
        maxCachedSessions: 2,
        defaultTtlMs: 60000,
      });

      smallCache.set('session-1', 'a', 'b', 'intent1');
      smallCache.set('session-2', 'a', 'b', 'intent2');
      smallCache.set('session-3', 'a', 'b', 'intent3');
      smallCache.set('session-4', 'a', 'b', 'intent4');

      expect(smallCache.getStats().evictions).toBe(2);
    });
  });

  describe('clear', () => {
    test('should clear specific session', () => {
      cache.set('session-1', 'alice', 'bob', 'weather/query');
      cache.set('session-2', 'alice', 'bob', 'calendar/book');

      const cleared = cache.clear('alice', 'bob', 'weather/query');

      expect(cleared).toBe(true);
      expect(cache.has('alice', 'bob', 'weather/query')).toBe(false);
      expect(cache.has('alice', 'bob', 'calendar/book')).toBe(true);
    });

    test('should return false when clearing non-existent session', () => {
      const cleared = cache.clear('alice', 'bob', 'nonexistent');

      expect(cleared).toBe(false);
    });
  });

  describe('clearByAmid', () => {
    test('should clear all sessions with a peer', () => {
      cache.set('session-1', 'alice', 'bob', 'intent1');
      cache.set('session-2', 'bob', 'alice', 'intent2');
      cache.set('session-3', 'alice', 'charlie', 'intent3');

      const cleared = cache.clearByAmid('bob');

      expect(cleared).toBe(2);
      expect(cache.has('alice', 'bob', 'intent1')).toBe(false);
      expect(cache.has('bob', 'alice', 'intent2')).toBe(false);
      expect(cache.has('alice', 'charlie', 'intent3')).toBe(true);
    });
  });

  describe('clearAll', () => {
    test('should clear all sessions', () => {
      cache.set('session-1', 'alice', 'bob', 'intent1');
      cache.set('session-2', 'alice', 'charlie', 'intent2');

      cache.clearAll();

      expect(cache.getStats().size).toBe(0);
      expect(cache.has('alice', 'bob', 'intent1')).toBe(false);
      expect(cache.has('alice', 'charlie', 'intent2')).toBe(false);
    });

    test('should emit cache_cleared event', () => {
      const handler = vi.fn();
      cache.onEvent(handler);

      cache.set('session-1', 'alice', 'bob', 'intent1');
      cache.clearAll();

      expect(handler).toHaveBeenCalledWith({
        type: 'cache_cleared',
        data: { clearedCount: 1 },
      });
    });
  });

  describe('getStats', () => {
    test('should track hits and misses', () => {
      cache.set('session-1', 'alice', 'bob', 'intent');

      cache.get('alice', 'bob', 'intent'); // hit
      cache.get('alice', 'bob', 'nonexistent'); // miss
      cache.get('alice', 'bob', 'intent'); // hit

      const stats = cache.getStats();
      expect(stats.hits).toBe(2);
      expect(stats.misses).toBe(1);
    });

    test('should track size', () => {
      cache.set('session-1', 'alice', 'bob', 'intent1');
      cache.set('session-2', 'alice', 'bob', 'intent2');

      expect(cache.getStats().size).toBe(2);

      cache.clear('alice', 'bob', 'intent1');

      expect(cache.getStats().size).toBe(1);
    });
  });

  describe('getAll', () => {
    test('should return all non-expired sessions', () => {
      cache.set('session-1', 'alice', 'bob', 'intent1');
      cache.set('session-2', 'alice', 'charlie', 'intent2');

      const all = cache.getAll();

      expect(all.length).toBe(2);
      expect(all.map(s => s.sessionId)).toContain('session-1');
      expect(all.map(s => s.sessionId)).toContain('session-2');
    });
  });

  describe('cleanup', () => {
    test('should remove expired sessions', () => {
      const shortCache = new SessionCache({
        maxCachedSessions: 10,
        defaultTtlMs: 50,
      });

      shortCache.set('session-1', 'alice', 'bob', 'intent1');
      shortCache.set('session-2', 'alice', 'charlie', 'intent2');

      return new Promise<void>((resolve) => {
        setTimeout(() => {
          const cleaned = shortCache.cleanup();
          expect(cleaned).toBe(2);
          expect(shortCache.getStats().size).toBe(0);
          resolve();
        }, 100);
      });
    });
  });

  describe('events', () => {
    test('should emit eviction event', () => {
      const smallCache = new SessionCache({
        maxCachedSessions: 1,
        defaultTtlMs: 60000,
      });

      const handler = vi.fn();
      smallCache.onEvent(handler);

      smallCache.set('session-1', 'alice', 'bob', 'intent1');
      smallCache.set('session-2', 'alice', 'charlie', 'intent2');

      expect(handler).toHaveBeenCalledWith(
        expect.objectContaining({
          type: 'eviction',
        })
      );
    });
  });
});
