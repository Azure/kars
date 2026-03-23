/**
 * Discovery layer for AgentMesh.
 * Handles agent registration and capability-based search.
 */

import { Identity } from './identity';
import { NetworkError } from './errors';
import { Tier } from './config';

/**
 * Information about a discovered agent.
 */
export interface AgentInfo {
  amid: string;
  tier: Tier | string;
  displayName?: string;
  organization?: string;
  signingPublicKey: string;
  exchangePublicKey: string;
  capabilities: string[];
  relayEndpoint: string;
  directEndpoint?: string;
  status: string;
  reputationScore: number;
  lastSeen: Date;
}

/**
 * Options for agent registration.
 */
export interface RegisterOptions {
  displayName?: string;
  capabilities?: string[];
  relayEndpoint?: string;
  directEndpoint?: string;
  verificationToken?: string;
}

/**
 * Options for capability search.
 */
export interface SearchOptions {
  capability: string;
  tierMin?: number;
  reputationMin?: number;
  status?: string;
  limit?: number;
  offset?: number;
}

/**
 * Prekey bundle for X3DH key exchange.
 */
export interface PrekeyBundle {
  identityKey: string;
  signedPrekey: string;
  signedPrekeySignature: string;
  signedPrekeyId: number;
  oneTimePrekeys: Array<{ id: number; key: string }>;
}

/**
 * Client for the AgentMesh registry API.
 */
export class RegistryClient {
  private readonly baseUrl: string;
  private readonly timeout: number;

  constructor(baseUrl = 'https://agentmesh.online/v1', timeout = 30000) {
    this.baseUrl = baseUrl.replace(/\/$/, '');
    this.timeout = timeout;
  }

  /**
   * Register this agent with the registry.
   */
  async register(
    identity: Identity,
    options: RegisterOptions = {}
  ): Promise<{ success: boolean; alreadyRegistered?: boolean; error?: string }> {
    const [timestamp, signature] = await identity.signTimestamp();

    const payload = {
      amid: identity.amid,
      signing_public_key: identity.signingPublicKeyB64,
      exchange_public_key: identity.exchangePublicKeyB64,
      display_name: options.displayName,
      capabilities: options.capabilities ?? [],
      relay_endpoint: options.relayEndpoint ?? 'wss://relay.agentmesh.online/v1/connect',
      direct_endpoint: options.directEndpoint,
      verification_token: options.verificationToken,
      timestamp,
      signature,
    };

    try {
      const response = await this.fetch('/registry/register', {
        method: 'POST',
        body: JSON.stringify(payload),
      });

      const data = await response.json();

      if (response.status === 201) {
        return { success: true, ...data };
      } else if (response.status === 409) {
        return { success: true, alreadyRegistered: true, ...data };
      } else {
        return { success: false, error: data.error ?? 'Unknown error' };
      }
    } catch (error) {
      throw new NetworkError(
        `Registry connection error: ${error instanceof Error ? error.message : 'Unknown error'}`,
        'REGISTRY_ERROR'
      );
    }
  }

  /**
   * Look up an agent by AMID.
   */
  async lookup(amid: string): Promise<AgentInfo | null> {
    try {
      const response = await this.fetch(`/registry/lookup?amid=${encodeURIComponent(amid)}`);

      if (response.status === 200) {
        const data = await response.json();
        return this.parseAgentInfo(data);
      } else if (response.status === 404) {
        return null;
      } else {
        throw new NetworkError(`Lookup failed: ${response.status}`, 'LOOKUP_ERROR', response.status);
      }
    } catch (error) {
      if (error instanceof NetworkError) throw error;
      throw new NetworkError(
        `Registry connection error: ${error instanceof Error ? error.message : 'Unknown error'}`,
        'REGISTRY_ERROR'
      );
    }
  }

  /**
   * Search for agents by capability.
   */
  async search(options: SearchOptions): Promise<{ results: AgentInfo[]; total: number }> {
    const params = new URLSearchParams();
    params.set('capability', options.capability);
    if (options.tierMin !== undefined) params.set('tier_min', String(options.tierMin));
    if (options.reputationMin !== undefined) params.set('reputation_min', String(options.reputationMin));
    if (options.status) params.set('status', options.status);
    params.set('limit', String(options.limit ?? 20));
    params.set('offset', String(options.offset ?? 0));

    try {
      const response = await this.fetch(`/registry/search?${params}`);

      if (response.status === 200) {
        const data = await response.json();
        return {
          results: data.results.map((r: Record<string, unknown>) => this.parseAgentInfo(r)),
          total: data.total,
        };
      } else {
        throw new NetworkError(`Search failed: ${response.status}`, 'SEARCH_ERROR', response.status);
      }
    } catch (error) {
      if (error instanceof NetworkError) throw error;
      throw new NetworkError(
        `Registry connection error: ${error instanceof Error ? error.message : 'Unknown error'}`,
        'REGISTRY_ERROR'
      );
    }
  }

  /**
   * Update agent presence status.
   */
  async updateStatus(identity: Identity, status: string): Promise<boolean> {
    const [timestamp, signature] = await identity.signTimestamp();

    const payload = {
      amid: identity.amid,
      status,
      timestamp,
      signature,
    };

    try {
      const response = await this.fetch('/registry/status', {
        method: 'POST',
        body: JSON.stringify(payload),
      });

      return response.status === 200;
    } catch {
      return false;
    }
  }

  /**
   * Update agent capabilities.
   */
  async updateCapabilities(identity: Identity, capabilities: string[]): Promise<boolean> {
    const [timestamp, signature] = await identity.signTimestamp();

    const payload = {
      amid: identity.amid,
      capabilities,
      timestamp,
      signature,
    };

    try {
      const response = await this.fetch('/registry/capabilities', {
        method: 'POST',
        body: JSON.stringify(payload),
      });

      return response.status === 200;
    } catch {
      return false;
    }
  }

  /**
   * Upload X3DH prekeys to registry.
   */
  async uploadPrekeys(
    identity: Identity,
    signedPrekey: string,
    signedPrekeySignature: string,
    signedPrekeyId: number,
    oneTimePrekeys: Array<{ id: number; key: string }>
  ): Promise<boolean> {
    const [timestamp, signature] = await identity.signTimestamp();

    const payload = {
      amid: identity.amid,
      signed_prekey: signedPrekey,
      signed_prekey_signature: signedPrekeySignature,
      signed_prekey_id: signedPrekeyId,
      one_time_prekeys: oneTimePrekeys,
      timestamp,
      signature,
    };

    try {
      const response = await this.fetch('/registry/prekeys', {
        method: 'POST',
        body: JSON.stringify(payload),
      });

      return response.status === 200;
    } catch {
      return false;
    }
  }

  /**
   * Fetch prekeys for an agent (for X3DH key exchange).
   */
  async getPrekeys(amid: string): Promise<PrekeyBundle | null> {
    try {
      const response = await this.fetch(`/registry/prekeys/${encodeURIComponent(amid)}`);

      if (response.status === 200) {
        const data = await response.json();
        return {
          identityKey: data.identity_key,
          signedPrekey: data.signed_prekey,
          signedPrekeySignature: data.signed_prekey_signature,
          signedPrekeyId: data.signed_prekey_id,
          oneTimePrekeys: data.one_time_prekeys ?? [],
        };
      } else if (response.status === 404) {
        return null;
      } else {
        throw new NetworkError(`Prekey fetch failed: ${response.status}`, 'PREKEY_ERROR', response.status);
      }
    } catch (error) {
      if (error instanceof NetworkError) throw error;
      return null;
    }
  }

  /**
   * Get available OAuth providers for tier verification.
   */
  async getOAuthProviders(): Promise<Array<{ name: string; displayName: string }>> {
    try {
      const response = await this.fetch('/auth/oauth/providers');

      if (response.status === 200) {
        const data = await response.json();
        return data.providers ?? [];
      } else {
        return [];
      }
    } catch {
      return [];
    }
  }

  /**
   * Start OAuth verification flow for tier upgrade.
   */
  async startOAuthVerification(
    identity: Identity,
    provider: string
  ): Promise<{ authorizationUrl: string; state: string } | null> {
    const [timestamp, signature] = await identity.signTimestamp();

    const payload = {
      amid: identity.amid,
      provider,
      timestamp,
      signature,
    };

    try {
      const response = await this.fetch('/auth/oauth/authorize', {
        method: 'POST',
        body: JSON.stringify(payload),
      });

      if (response.status === 200) {
        const data = await response.json();
        return {
          authorizationUrl: data.authorization_url,
          state: data.state,
        };
      } else {
        return null;
      }
    } catch {
      return null;
    }
  }

  /**
   * Get verification status for an agent.
   */
  async getVerificationStatus(amid: string): Promise<{ tier: string; isVerified: boolean } | null> {
    const info = await this.lookup(amid);
    if (!info) return null;

    return {
      tier: info.tier as string,
      isVerified: info.tier === Tier.VERIFIED || info.tier === Tier.ORGANIZATION,
    };
  }

  /**
   * Check if an agent's certificate has been revoked.
   */
  async checkRevocation(amid: string): Promise<{ revoked: boolean; reason?: string }> {
    try {
      const response = await this.fetch(`/registry/revocation?amid=${encodeURIComponent(amid)}`);

      if (response.status === 200) {
        return await response.json();
      } else {
        return { revoked: false };
      }
    } catch {
      return { revoked: false };
    }
  }

  /**
   * Check revocation status for multiple agents at once.
   */
  async bulkCheckRevocation(amids: string[]): Promise<Record<string, { revoked: boolean; reason?: string }>> {
    try {
      const response = await this.fetch('/registry/revocations/bulk', {
        method: 'POST',
        body: JSON.stringify({ amids }),
      });

      if (response.status === 200) {
        const data = await response.json();
        return data.revocations ?? {};
      } else {
        return {};
      }
    } catch {
      return {};
    }
  }

  /**
   * Check registry health.
   */
  async healthCheck(): Promise<{ status: string; agentCount?: number }> {
    try {
      const response = await this.fetch('/health');

      if (response.status === 200) {
        return await response.json();
      } else {
        return { status: 'unhealthy' };
      }
    } catch {
      return { status: 'unreachable' };
    }
  }

  /**
   * Submit reputation feedback for another agent.
   */
  async submitReputation(
    identity: Identity,
    targetAmid: string,
    sessionId: string,
    score: number,
    tags?: string[]
  ): Promise<boolean> {
    if (score < 0.0 || score > 1.0) {
      throw new Error('Score must be between 0.0 and 1.0');
    }

    const [timestamp, signature] = await identity.signTimestamp();

    const payload = {
      target_amid: targetAmid,
      from_amid: identity.amid,
      session_id: sessionId,
      score,
      tags,
      timestamp,
      signature,
    };

    try {
      const response = await this.fetch('/registry/reputation', {
        method: 'POST',
        body: JSON.stringify(payload),
      });

      return response.status === 200;
    } catch {
      return false;
    }
  }

  /**
   * Internal fetch helper with timeout.
   */
  private async fetch(path: string, options: RequestInit = {}): Promise<Response> {
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), this.timeout);

    try {
      const response = await fetch(`${this.baseUrl}${path}`, {
        ...options,
        headers: {
          'Content-Type': 'application/json',
          ...options.headers,
        },
        signal: controller.signal,
      });

      return response;
    } finally {
      clearTimeout(timeoutId);
    }
  }

  /**
   * Parse agent info from API response.
   * Handles both camelCase and snake_case field names for compatibility.
   */
  private parseAgentInfo(data: Record<string, unknown>): AgentInfo {
    // Helper to get value from either camelCase or snake_case key
    const get = <T>(camel: string, snake: string): T | undefined =>
      (data[camel] ?? data[snake]) as T | undefined;

    const lastSeenStr = get<string>('lastSeen', 'last_seen') ?? new Date().toISOString();
    // Handle 'Z' suffix for UTC timezone
    const lastSeen = new Date(lastSeenStr.endsWith('Z') ? lastSeenStr : lastSeenStr.replace('Z', '+00:00'));

    return {
      amid: data.amid as string,
      tier: data.tier as Tier,
      displayName: get<string>('displayName', 'display_name'),
      organization: data.organization as string | undefined,
      signingPublicKey: get<string>('signingPublicKey', 'signing_public_key') ?? '',
      exchangePublicKey: get<string>('exchangePublicKey', 'exchange_public_key') ?? '',
      capabilities: (data.capabilities as string[]) ?? [],
      relayEndpoint: get<string>('relayEndpoint', 'relay_endpoint') ?? '',
      directEndpoint: get<string>('directEndpoint', 'direct_endpoint'),
      status: data.status as string,
      reputationScore: get<number>('reputationScore', 'reputation_score') ?? 0.5,
      lastSeen,
    };
  }
}
