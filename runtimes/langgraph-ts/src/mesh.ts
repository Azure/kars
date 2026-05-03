// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * AgentMesh bridge for the in-pod adapter.
 *
 * The router sidecar reverse-proxies the AgentMesh relay/registry at
 * `/agt/relay` and `/agt/registry` so we have one endpoint for
 * governance/auth. This module is a thin transport over those
 * endpoints. It serializes a TaskEnvelope-shaped object for outbound
 * messages and returns inbox payloads as plain dicts (we keep
 * envelopes opaque to the user — they only see the content).
 *
 * Wire format mirrors the upstream `a2a_agentmesh` schema (camelCase
 * keys converted to snake_case where the relay expects it).
 */

export const DEFAULT_RELAY_URL = 'http://127.0.0.1:8443/agt/relay/';
export const DEFAULT_REGISTRY_URL = 'http://127.0.0.1:8443/agt/registry/';
export const DEFAULT_TIMEOUT_MS = 10_000;

export const ENV_AGENT_DID = 'AZURECLAW_AGENT_DID';
export const ENV_AGENT_NAME = 'AZURECLAW_AGENT_NAME';

function envDid(): string {
  return process.env[ENV_AGENT_DID] ?? 'did:mesh:unknown';
}

export interface TaskEnvelope {
  task_id: string;
  state: string;
  skill_id: string;
  source_did: string;
  target_did: string;
  messages: Array<{
    role: string;
    parts: Array<{ type: string; text: string }>;
  }>;
  created_at: number;
}

function buildEnvelope(
  targetDid: string,
  content: string,
  skillId: string,
): TaskEnvelope {
  return {
    task_id: `task-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`,
    state: 'submitted',
    skill_id: skillId,
    source_did: envDid(),
    target_did: targetDid,
    messages: [
      { role: 'user', parts: [{ type: 'text/plain', text: content }] },
    ],
    created_at: Date.now() / 1000,
  };
}

function joinUrl(base: string, path: string): string {
  const b = base.endsWith('/') ? base : `${base}/`;
  const p = path.startsWith('/') ? path.slice(1) : path;
  return `${b}${p}`;
}

export interface MeshClientOptions {
  relayUrl?: string;
  registryUrl?: string;
  timeoutMs?: number;
  /** Test hook — override fetch implementation. */
  fetchImpl?: typeof fetch;
}

export class MeshClient {
  readonly relayUrl: string;
  readonly registryUrl: string;
  private readonly timeoutMs: number;
  private readonly fetchImpl: typeof fetch;

  constructor(opts: MeshClientOptions = {}) {
    const relay =
      opts.relayUrl ?? process.env.AZURECLAW_AGT_RELAY_URL ?? DEFAULT_RELAY_URL;
    const registry =
      opts.registryUrl ??
      process.env.AZURECLAW_AGT_REGISTRY_URL ??
      DEFAULT_REGISTRY_URL;
    this.relayUrl = relay.endsWith('/') ? relay : `${relay}/`;
    this.registryUrl = registry.endsWith('/') ? registry : `${registry}/`;
    this.timeoutMs = opts.timeoutMs ?? DEFAULT_TIMEOUT_MS;
    this.fetchImpl = opts.fetchImpl ?? fetch;
  }

  private async request(
    url: string,
    init: RequestInit,
  ): Promise<unknown> {
    const ctrl = new AbortController();
    const timer = setTimeout(() => ctrl.abort(), this.timeoutMs);
    try {
      const res = await this.fetchImpl(url, { ...init, signal: ctrl.signal });
      if (!res.ok) {
        const body = await res.text().catch(() => '');
        throw new Error(
          `mesh request failed: ${res.status} ${res.statusText} ${body}`,
        );
      }
      // 404 from /lookup is handled by caller.
      const text = await res.text();
      return text ? JSON.parse(text) : null;
    } finally {
      clearTimeout(timer);
    }
  }

  async send(
    targetAgent: string,
    content: string,
    opts: { skillId?: string } = {},
  ): Promise<Record<string, unknown>> {
    const envelope = buildEnvelope(
      targetAgent,
      content,
      opts.skillId ?? 'chat',
    );
    const url = joinUrl(this.relayUrl, 'send');
    const body = (await this.request(url, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(envelope),
    })) as Record<string, unknown> | null;
    return body ?? {};
  }

  async receive(): Promise<Array<Record<string, unknown>>> {
    const url = `${joinUrl(this.relayUrl, 'inbox')}?agent_did=${encodeURIComponent(envDid())}`;
    const body = (await this.request(url, { method: 'GET' })) as
      | { messages?: Array<Record<string, unknown>> }
      | Array<Record<string, unknown>>
      | null;
    if (Array.isArray(body)) {
      return body;
    }
    if (body && Array.isArray(body.messages)) {
      return body.messages;
    }
    return [];
  }

  async lookup(
    agentName: string,
  ): Promise<Record<string, unknown> | null> {
    const url = `${joinUrl(this.registryUrl, 'lookup')}?name=${encodeURIComponent(agentName)}`;
    const ctrl = new AbortController();
    const timer = setTimeout(() => ctrl.abort(), this.timeoutMs);
    try {
      const res = await this.fetchImpl(url, {
        method: 'GET',
        signal: ctrl.signal,
      });
      if (res.status === 404) {
        return null;
      }
      if (!res.ok) {
        const body = await res.text().catch(() => '');
        throw new Error(
          `registry lookup failed: ${res.status} ${res.statusText} ${body}`,
        );
      }
      return (await res.json()) as Record<string, unknown>;
    } finally {
      clearTimeout(timer);
    }
  }
}

let defaultClient: MeshClient | undefined;

function client(): MeshClient {
  if (!defaultClient) {
    defaultClient = new MeshClient();
  }
  return defaultClient;
}

export async function sendMessage(
  targetAgent: string,
  content: string,
  opts: { skillId?: string } = {},
): Promise<Record<string, unknown>> {
  return client().send(targetAgent, content, opts);
}

export async function receiveMessages(): Promise<
  Array<Record<string, unknown>>
> {
  return client().receive();
}

/** Test hook — drop the cached MeshClient. */
export function resetDefaultClient(): void {
  defaultClient = undefined;
}
