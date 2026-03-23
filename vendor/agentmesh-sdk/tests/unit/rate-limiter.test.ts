/**
 * Unit tests for RateLimiter module.
 */
import { describe, test, expect, vi } from 'vitest';
import { RateLimiter, RateLimitError } from '../../src/rate-limiter';

describe('RateLimiter', () => {
  describe('constructor', () => {
    test('should create rate limiter with default config', () => {
      const limiter = new RateLimiter();
      const status = limiter.getStatus();

      expect(status.maxTokens).toBe(500); // default maxBurst
    });

    test('should create rate limiter with custom config', () => {
      const limiter = new RateLimiter({
        maxPerSecond: 10,
        maxBurst: 50,
      });
      const status = limiter.getStatus();

      expect(status.maxTokens).toBe(50);
    });
  });

  describe('consume', () => {
    test('should consume tokens successfully', () => {
      const limiter = new RateLimiter({ maxBurst: 10 });

      expect(() => limiter.consume()).not.toThrow();
      expect(() => limiter.consume()).not.toThrow();
    });

    test('should throw RateLimitError when exhausted', () => {
      const limiter = new RateLimiter({
        maxPerSecond: 1,
        maxBurst: 2,
      });

      limiter.consume();
      limiter.consume();

      expect(() => limiter.consume()).toThrow(RateLimitError);
    });

    test('should include retryAfter in error', () => {
      const limiter = new RateLimiter({
        maxPerSecond: 1,
        maxBurst: 1,
      });

      limiter.consume();

      try {
        limiter.consume();
        expect.fail('Should have thrown');
      } catch (error) {
        expect(error).toBeInstanceOf(RateLimitError);
        expect((error as RateLimitError).retryAfter).toBeGreaterThan(0);
      }
    });
  });

  describe('canConsume', () => {
    test('should return true when tokens available', () => {
      const limiter = new RateLimiter({ maxBurst: 10 });

      expect(limiter.canConsume()).toBe(true);
    });

    test('should return false when exhausted', () => {
      const limiter = new RateLimiter({
        maxPerSecond: 1,
        maxBurst: 1,
      });

      limiter.consume();

      expect(limiter.canConsume()).toBe(false);
    });
  });

  describe('per-peer limiting', () => {
    test('should track per-peer limits separately', () => {
      const limiter = new RateLimiter({
        maxBurst: 100,
        perPeer: { maxPerSecond: 2 },
      });

      // Consume peer1's quota
      limiter.consume('peer1');
      limiter.consume('peer1');

      // peer1 should be limited but peer2 shouldn't
      expect(limiter.canConsume('peer1')).toBe(false);
      expect(limiter.canConsume('peer2')).toBe(true);
    });

    test('should throw when peer limit exceeded', () => {
      const limiter = new RateLimiter({
        maxBurst: 100,
        perPeer: { maxPerSecond: 1 },
      });

      limiter.consume('peer1');

      expect(() => limiter.consume('peer1')).toThrow(RateLimitError);
    });
  });

  describe('token refill', () => {
    test('should refill tokens over time', async () => {
      const limiter = new RateLimiter({
        maxPerSecond: 100,
        maxBurst: 1,
      });

      limiter.consume();
      expect(limiter.canConsume()).toBe(false);

      // Wait for refill (10ms = 1 token at 100/sec)
      await new Promise(resolve => setTimeout(resolve, 20));

      expect(limiter.canConsume()).toBe(true);
    });
  });

  describe('getRetryAfter', () => {
    test('should return 0 when tokens available', () => {
      const limiter = new RateLimiter({ maxBurst: 10 });

      expect(limiter.getRetryAfter()).toBe(0);
    });

    test('should return positive time when exhausted', () => {
      const limiter = new RateLimiter({
        maxPerSecond: 10,
        maxBurst: 1,
      });

      limiter.consume();

      const retryAfter = limiter.getRetryAfter();
      expect(retryAfter).toBeGreaterThan(0);
    });
  });

  describe('getStatus', () => {
    test('should return current status', () => {
      const limiter = new RateLimiter({
        maxPerSecond: 10,
        maxBurst: 5,
      });

      const status = limiter.getStatus();

      expect(status.tokens).toBe(5);
      expect(status.maxTokens).toBe(5);
    });

    test('should track peer statuses', () => {
      const limiter = new RateLimiter({
        maxBurst: 100,
        perPeer: { maxPerSecond: 10 },
      });

      limiter.consume('peer1');
      limiter.consume('peer2');

      const status = limiter.getStatus();
      expect(status.peerStatuses.size).toBe(2);
      expect(status.peerStatuses.has('peer1')).toBe(true);
      expect(status.peerStatuses.has('peer2')).toBe(true);
    });
  });

  describe('waitForCapacity', () => {
    test('should return true immediately when capacity available', async () => {
      const limiter = new RateLimiter({ maxBurst: 10 });

      const result = await limiter.waitForCapacity(1000);

      expect(result).toBe(true);
    });

    test('should wait and return true when capacity becomes available', async () => {
      const limiter = new RateLimiter({
        maxPerSecond: 1000,
        maxBurst: 1,
      });

      limiter.consume();

      const result = await limiter.waitForCapacity(100);

      expect(result).toBe(true);
    });

    test('should return false when timeout exceeded', async () => {
      const limiter = new RateLimiter({
        maxPerSecond: 1,
        maxBurst: 1,
      });

      limiter.consume();

      const result = await limiter.waitForCapacity(10);

      expect(result).toBe(false);
    });
  });

  describe('events', () => {
    test('should emit rate_limited event on global limit', () => {
      const limiter = new RateLimiter({
        maxPerSecond: 1,
        maxBurst: 1,
      });

      const handler = vi.fn();
      limiter.onEvent(handler);

      limiter.consume();

      try {
        limiter.consume();
      } catch {
        // Expected
      }

      expect(handler).toHaveBeenCalledWith({
        type: 'rate_limited',
        data: expect.objectContaining({
          scope: 'global',
        }),
      });
    });

    test('should emit rate_limited event on peer limit', () => {
      const limiter = new RateLimiter({
        maxBurst: 100,
        perPeer: { maxPerSecond: 1 },
      });

      const handler = vi.fn();
      limiter.onEvent(handler);

      limiter.consume('peer1');

      try {
        limiter.consume('peer1');
      } catch {
        // Expected
      }

      expect(handler).toHaveBeenCalledWith({
        type: 'rate_limited',
        data: expect.objectContaining({
          scope: 'peer',
          peerAmid: 'peer1',
        }),
      });
    });
  });
});
