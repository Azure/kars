/**
 * Rate Limiter for AgentMesh.
 * Token bucket algorithm for client-side rate limiting.
 */

/**
 * Rate limit configuration.
 */
export interface RateLimitConfig {
  /** Max tokens per second (refill rate) */
  maxPerSecond?: number;
  /** Max burst capacity */
  maxBurst?: number;
  /** Per-peer rate limit */
  perPeer?: {
    maxPerSecond?: number;
  };
}

/**
 * Rate limit status.
 */
export interface RateLimitStatus {
  /** Current token count */
  tokens: number;
  /** Maximum tokens */
  maxTokens: number;
  /** Refill rate per second */
  refillRate: number;
  /** Per-peer statuses */
  peerStatuses: Map<string, { tokens: number; maxTokens: number }>;
}

/**
 * Error thrown when rate limit is exceeded.
 */
export class RateLimitError extends Error {
  readonly code = 'RATE_LIMITED';
  readonly retryAfter: number;

  constructor(retryAfterMs: number) {
    super(`Rate limit exceeded. Retry after ${retryAfterMs}ms`);
    this.name = 'RateLimitError';
    this.retryAfter = retryAfterMs;
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

/**
 * Token bucket for rate limiting.
 */
class TokenBucket {
  private tokens: number;
  private lastRefill: number;
  private readonly maxTokens: number;
  private readonly refillRate: number; // tokens per millisecond

  constructor(maxTokens: number, tokensPerSecond: number) {
    this.maxTokens = maxTokens;
    this.tokens = maxTokens;
    this.refillRate = tokensPerSecond / 1000;
    this.lastRefill = Date.now();
  }

  /**
   * Try to consume a token.
   * Returns true if successful, false if rate limited.
   */
  tryConsume(): boolean {
    this.refill();

    if (this.tokens >= 1) {
      this.tokens -= 1;
      return true;
    }

    return false;
  }

  /**
   * Get time until next token is available (in ms).
   */
  getRetryAfter(): number {
    this.refill();

    if (this.tokens >= 1) {
      return 0;
    }

    // Calculate time to refill 1 token
    return Math.ceil((1 - this.tokens) / this.refillRate);
  }

  /**
   * Get current status.
   */
  getStatus(): { tokens: number; maxTokens: number } {
    this.refill();
    return {
      tokens: Math.floor(this.tokens),
      maxTokens: this.maxTokens,
    };
  }

  /**
   * Refill tokens based on time elapsed.
   */
  private refill(): void {
    const now = Date.now();
    const elapsed = now - this.lastRefill;
    const tokensToAdd = elapsed * this.refillRate;

    this.tokens = Math.min(this.maxTokens, this.tokens + tokensToAdd);
    this.lastRefill = now;
  }
}

type RateLimitEventHandler = (event: { type: string; data: unknown }) => void;

/**
 * Rate limiter with global and per-peer limits.
 */
export class RateLimiter {
  private readonly globalBucket: TokenBucket;
  private readonly peerBuckets: Map<string, TokenBucket> = new Map();
  private readonly peerMaxTokens: number;
  private readonly peerRefillRate: number;
  private readonly eventHandlers: RateLimitEventHandler[] = [];

  constructor(config: RateLimitConfig = {}) {
    const maxPerSecond = config.maxPerSecond ?? 100;
    const maxBurst = config.maxBurst ?? 500;

    this.globalBucket = new TokenBucket(maxBurst, maxPerSecond);

    this.peerMaxTokens = config.perPeer?.maxPerSecond ?? 50;
    this.peerRefillRate = config.perPeer?.maxPerSecond ?? 50;
  }

  /**
   * Check if a message can be sent (consume token).
   * @param peerAmid - Optional peer AMID for per-peer limiting
   * @throws RateLimitError if rate limit exceeded
   */
  consume(peerAmid?: string): void {
    // Check global limit first
    if (!this.globalBucket.tryConsume()) {
      const retryAfter = this.globalBucket.getRetryAfter();
      this.emitEvent('rate_limited', {
        scope: 'global',
        peerAmid,
        retryAfter,
      });
      throw new RateLimitError(retryAfter);
    }

    // Check per-peer limit if applicable
    if (peerAmid) {
      const peerBucket = this.getOrCreatePeerBucket(peerAmid);
      if (!peerBucket.tryConsume()) {
        const retryAfter = peerBucket.getRetryAfter();
        this.emitEvent('rate_limited', {
          scope: 'peer',
          peerAmid,
          retryAfter,
        });
        throw new RateLimitError(retryAfter);
      }
    }
  }

  /**
   * Check if a message can be sent without consuming.
   */
  canConsume(peerAmid?: string): boolean {
    const globalStatus = this.globalBucket.getStatus();
    if (globalStatus.tokens < 1) {
      return false;
    }

    if (peerAmid) {
      const peerBucket = this.getOrCreatePeerBucket(peerAmid);
      const peerStatus = peerBucket.getStatus();
      if (peerStatus.tokens < 1) {
        return false;
      }
    }

    return true;
  }

  /**
   * Get retry time if rate limited.
   */
  getRetryAfter(peerAmid?: string): number {
    const globalRetry = this.globalBucket.getRetryAfter();

    if (!peerAmid) {
      return globalRetry;
    }

    const peerBucket = this.getOrCreatePeerBucket(peerAmid);
    const peerRetry = peerBucket.getRetryAfter();

    return Math.max(globalRetry, peerRetry);
  }

  /**
   * Get current rate limit status.
   */
  getStatus(): RateLimitStatus {
    const globalStatus = this.globalBucket.getStatus();
    const peerStatuses = new Map<string, { tokens: number; maxTokens: number }>();

    for (const [amid, bucket] of this.peerBuckets) {
      peerStatuses.set(amid, bucket.getStatus());
    }

    return {
      tokens: globalStatus.tokens,
      maxTokens: globalStatus.maxTokens,
      refillRate: this.peerRefillRate,
      peerStatuses,
    };
  }

  /**
   * Register an event handler.
   */
  onEvent(handler: RateLimitEventHandler): void {
    this.eventHandlers.push(handler);
  }

  /**
   * Wait for capacity to become available.
   * @param maxWaitMs - Maximum time to wait
   * @param peerAmid - Optional peer for per-peer limit
   * @returns true if capacity available, false if timeout
   */
  async waitForCapacity(maxWaitMs: number, peerAmid?: string): Promise<boolean> {
    const startTime = Date.now();

    while (Date.now() - startTime < maxWaitMs) {
      if (this.canConsume(peerAmid)) {
        return true;
      }

      // Wait a bit before checking again
      const retryAfter = Math.min(
        this.getRetryAfter(peerAmid),
        maxWaitMs - (Date.now() - startTime)
      );

      if (retryAfter <= 0) {
        break;
      }

      await new Promise(resolve => setTimeout(resolve, retryAfter));
    }

    return this.canConsume(peerAmid);
  }

  /**
   * Get or create a peer bucket.
   */
  private getOrCreatePeerBucket(peerAmid: string): TokenBucket {
    let bucket = this.peerBuckets.get(peerAmid);
    if (!bucket) {
      bucket = new TokenBucket(this.peerMaxTokens, this.peerRefillRate);
      this.peerBuckets.set(peerAmid, bucket);
    }
    return bucket;
  }

  /**
   * Emit an event to handlers.
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
