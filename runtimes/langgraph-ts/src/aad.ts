// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * AAD token broker for the in-pod adapter.
 *
 * Acquires bearer tokens via Azure Workload Identity (AKS) using
 * `@azure/identity`'s `WorkloadIdentityCredential`. Tokens are cached
 * by scope and refreshed when within `SKEW_SECONDS` of expiry so no
 * caller pays the IMDS round-trip on the hot path.
 *
 * The router sidecar handles the LLM-side credential exchange — this
 * broker is for *in-process* needs (e.g. signed mesh envelopes,
 * attestation of a sub-agent spawn). The two paths must remain
 * independent.
 */

export const DEFAULT_SCOPE = 'https://cognitiveservices.azure.com/.default';
const SKEW_SECONDS = 300; // refresh 5 minutes before token expiry

export interface AccessTokenLike {
  token: string;
  /** Unix seconds. */
  expiresOnTimestamp: number;
}

export interface CredentialLike {
  getToken(scopes: string | string[]): Promise<AccessTokenLike | null>;
}

interface CachedToken {
  token: string;
  expiresOn: number;
}

export class TokenBroker {
  private credential: CredentialLike | undefined;
  private readonly skew: number;
  private readonly cache = new Map<string, CachedToken>();
  private readonly inflight = new Map<string, Promise<string>>();

  constructor(credential?: CredentialLike, skewSeconds: number = SKEW_SECONDS) {
    this.credential = credential;
    this.skew = skewSeconds;
  }

  private async ensureCredential(): Promise<CredentialLike> {
    if (this.credential) {
      return this.credential;
    }
    // Lazy import keeps unit tests cheap when an injected fake is used.
    const mod = await import('@azure/identity');
    this.credential = new mod.WorkloadIdentityCredential();
    return this.credential;
  }

  async getToken(scope: string = DEFAULT_SCOPE): Promise<string> {
    const now = Math.floor(Date.now() / 1000);
    const cached = this.cache.get(scope);
    if (cached && cached.expiresOn - this.skew > now) {
      return cached.token;
    }
    const inflight = this.inflight.get(scope);
    if (inflight) {
      return inflight;
    }
    const promise = this.fetchToken(scope).finally(() => {
      this.inflight.delete(scope);
    });
    this.inflight.set(scope, promise);
    return promise;
  }

  private async fetchToken(scope: string): Promise<string> {
    const cred = await this.ensureCredential();
    const access = await cred.getToken(scope);
    if (!access) {
      throw new Error(`AAD token acquisition returned null for scope ${scope}`);
    }
    this.cache.set(scope, {
      token: access.token,
      expiresOn: Math.floor(access.expiresOnTimestamp / 1000),
    });
    return access.token;
  }

  invalidate(scope?: string): void {
    if (scope === undefined) {
      this.cache.clear();
    } else {
      this.cache.delete(scope);
    }
  }
}

let defaultBroker: TokenBroker | undefined;

function broker(): TokenBroker {
  if (!defaultBroker) {
    defaultBroker = new TokenBroker();
  }
  return defaultBroker;
}

export async function getToken(scope: string = DEFAULT_SCOPE): Promise<string> {
  return broker().getToken(scope);
}

/** Test hook — drop the process-wide singleton. */
export function resetDefaultBroker(): void {
  defaultBroker = undefined;
}
