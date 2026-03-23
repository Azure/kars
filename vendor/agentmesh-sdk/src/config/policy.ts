/**
 * Configuration and policy management for AgentMesh SDK.
 */

/**
 * Agent tier levels.
 */
export enum Tier {
  /** Unverified agent - basic capabilities */
  ANONYMOUS = 'anonymous',
  /** Email-verified agent */
  VERIFIED = 'verified',
  /** Organization-verified agent */
  ORGANIZATION = 'organization',
}

/**
 * Tier numeric values for comparison.
 */
export const TierLevel: Record<Tier, number> = {
  [Tier.ANONYMOUS]: 0,
  [Tier.VERIFIED]: 1,
  [Tier.ORGANIZATION]: 2,
};

/**
 * Get numeric level for a tier.
 */
export function getTierLevel(tier: Tier | string): number {
  return TierLevel[tier as Tier] ?? 0;
}

/**
 * Policy options for KNOCK acceptance.
 */
export interface PolicyOptions {
  /** Minimum tier level to accept (default: anonymous) */
  minTier?: Tier;
  /** Minimum reputation score to accept (0.0 - 1.0, default: 0.0) */
  minReputation?: number;
  /** Allowed intent categories (empty = allow all) */
  allowedIntents?: string[];
  /** Blocked intent categories */
  blockedIntents?: string[];
  /** Allowed source AMIDs (empty = allow all) */
  allowedAmids?: string[];
  /** Blocked source AMIDs */
  blockedAmids?: string[];
  /** Auto-accept KNOCKs matching this policy */
  autoAccept?: boolean;
  /** Maximum session TTL in seconds (default: 300) */
  maxSessionTtl?: number;
  /** Maximum concurrent sessions (default: 100) */
  maxConcurrentSessions?: number;
}

/**
 * Policy for evaluating incoming KNOCK messages.
 */
export class Policy {
  readonly minTier: Tier;
  readonly minReputation: number;
  readonly allowedIntents: Set<string>;
  readonly blockedIntents: Set<string>;
  readonly allowedAmids: Set<string>;
  readonly blockedAmids: Set<string>;
  readonly autoAccept: boolean;
  readonly maxSessionTtl: number;
  readonly maxConcurrentSessions: number;

  constructor(options: PolicyOptions = {}) {
    this.minTier = options.minTier ?? Tier.ANONYMOUS;
    this.minReputation = options.minReputation ?? 0.0;
    this.allowedIntents = new Set(options.allowedIntents ?? []);
    this.blockedIntents = new Set(options.blockedIntents ?? []);
    this.allowedAmids = new Set(options.allowedAmids ?? []);
    this.blockedAmids = new Set(options.blockedAmids ?? []);
    this.autoAccept = options.autoAccept ?? false;
    this.maxSessionTtl = options.maxSessionTtl ?? 300;
    this.maxConcurrentSessions = options.maxConcurrentSessions ?? 100;
  }

  /**
   * Evaluate whether a KNOCK should be accepted based on this policy.
   */
  evaluate(knock: KnockContext): PolicyResult {
    // Check blocked AMID
    if (this.blockedAmids.has(knock.fromAmid)) {
      return { allowed: false, reason: 'AMID is blocked' };
    }

    // Check allowed AMID (if list is non-empty)
    if (this.allowedAmids.size > 0 && !this.allowedAmids.has(knock.fromAmid)) {
      return { allowed: false, reason: 'AMID not in allow list' };
    }

    // Check tier
    if (getTierLevel(knock.fromTier) < getTierLevel(this.minTier)) {
      return { allowed: false, reason: `Tier ${knock.fromTier} below minimum ${this.minTier}` };
    }

    // Check reputation
    if (knock.fromReputation < this.minReputation) {
      return {
        allowed: false,
        reason: `Reputation ${knock.fromReputation} below minimum ${this.minReputation}`,
      };
    }

    // Check blocked intent
    const intentCategory = knock.intentCategory;
    if (intentCategory && this.blockedIntents.has(intentCategory)) {
      return { allowed: false, reason: `Intent category ${intentCategory} is blocked` };
    }

    // Check allowed intent (if list is non-empty)
    if (this.allowedIntents.size > 0 && intentCategory && !this.allowedIntents.has(intentCategory)) {
      return { allowed: false, reason: `Intent category ${intentCategory} not in allow list` };
    }

    // Check session TTL
    if (knock.requestedTtl > this.maxSessionTtl) {
      return {
        allowed: false,
        reason: `Requested TTL ${knock.requestedTtl}s exceeds maximum ${this.maxSessionTtl}s`,
      };
    }

    return {
      allowed: true,
      autoAccept: this.autoAccept,
    };
  }

  /**
   * Create a permissive policy that accepts all KNOCKs.
   */
  static permissive(): Policy {
    return new Policy({ autoAccept: true });
  }

  /**
   * Create a restrictive policy that only accepts verified agents.
   */
  static verified(): Policy {
    return new Policy({
      minTier: Tier.VERIFIED,
      minReputation: 0.5,
    });
  }

  /**
   * Create a policy that only accepts organization-verified agents.
   */
  static organization(): Policy {
    return new Policy({
      minTier: Tier.ORGANIZATION,
      minReputation: 0.7,
    });
  }
}

/**
 * Context for policy evaluation.
 */
export interface KnockContext {
  fromAmid: string;
  fromTier: Tier | string;
  fromReputation: number;
  intentCategory?: string;
  requestedTtl: number;
}

/**
 * Result of policy evaluation.
 */
export interface PolicyResult {
  allowed: boolean;
  reason?: string;
  autoAccept?: boolean;
}

/**
 * Configuration options for the AgentMesh client.
 */
export interface ConfigOptions {
  /** Registry API base URL */
  registryUrl?: string;
  /** Relay WebSocket URL */
  relayUrl?: string;
  /** Policy for accepting incoming KNOCKs */
  policy?: Policy;
  /** Agent display name for registration */
  displayName?: string;
  /** Agent capabilities for registration */
  capabilities?: string[];
  /** Enable P2P transport (if available) */
  enableP2P?: boolean;
  /** Connection timeout in milliseconds */
  connectionTimeout?: number;
  /** Reconnection attempts before giving up */
  maxReconnectAttempts?: number;
  /** Enable debug logging */
  debug?: boolean;
}

/**
 * Client configuration.
 */
export class Config {
  readonly registryUrl: string;
  readonly relayUrl: string;
  readonly policy: Policy;
  readonly displayName?: string;
  readonly capabilities: string[];
  readonly enableP2P: boolean;
  readonly connectionTimeout: number;
  readonly maxReconnectAttempts: number;
  readonly debug: boolean;

  constructor(options: ConfigOptions = {}) {
    this.registryUrl = options.registryUrl ?? 'https://agentmesh.online/v1';
    this.relayUrl = options.relayUrl ?? 'wss://relay.agentmesh.online/v1/connect';
    this.policy = options.policy ?? new Policy();
    this.displayName = options.displayName;
    this.capabilities = options.capabilities ?? [];
    this.enableP2P = options.enableP2P ?? false;
    this.connectionTimeout = options.connectionTimeout ?? 30000;
    this.maxReconnectAttempts = options.maxReconnectAttempts ?? 5;
    this.debug = options.debug ?? false;
  }

  /**
   * Create configuration with defaults.
   */
  static default(): Config {
    return new Config();
  }

  /**
   * Create configuration for development.
   */
  static development(options: Partial<ConfigOptions> = {}): Config {
    return new Config({
      debug: true,
      policy: Policy.permissive(),
      ...options,
    });
  }
}
