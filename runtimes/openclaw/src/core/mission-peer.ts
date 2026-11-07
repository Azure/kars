// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { IMeshRegistry, RegistryEntry, RegistryRequestOptions } from "./mesh-registry.js";

export const PEER_TOOLS = ["mesh_send", "discover", "mesh_inbox", "mesh_await"] as const;
export type PeerToolName = typeof PEER_TOOLS[number];
export type PeerOperation = NonNullable<RegistryRequestOptions["operation"]>;
const RESULT_BYTES = 160 * 1024;
const did = (value: string) => /^did:mesh:[a-f0-9]{32}$/.test(value);
const peer = (value: unknown): value is string => typeof value === "string" && value.length > 0 && value.length <= 253
  && !/\s|\p{Cc}/u.test(value) && (!value.startsWith("did:") || did(value));
const record = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);

/** Encoded protocol envelopes are not ordinary text, including envelopes nested in content. */
export function ordinaryPeerMessage(value: unknown, depth = 0): boolean {
  if (depth > 16) return false;
  if (typeof value === "string") {
    if (Buffer.byteLength(value) > RESULT_BYTES) return false;
    if (!/^\s*[[{"]/.test(value)) return true;
    try { return ordinaryPeerMessage(JSON.parse(value), depth + 1); }
    catch { return true; }
  }
  if (Array.isArray(value)) return value.every(item => ordinaryPeerMessage(item, depth + 1));
  if (!record(value)) return true;
  if (value.type !== undefined && value.type !== "message" && value.type !== "text") return false;
  return Object.values(value).every(item => ordinaryPeerMessage(item, depth + 1));
}

export function peerSchema(name: PeerToolName): Record<string, unknown> {
  const target = { type: "string", minLength: 1, maxLength: 253 };
  return { type: "object", additionalProperties: false,
    properties: name === "mesh_send" ? { to_agent: target, content: { type: "string", maxLength: 65536 } }
      : name === "discover" ? {
        name: { ...target, type: ["string", "null"], description: "Exact display name, or null when looking up a DID." },
        did: { ...target, type: ["string", "null"], description: "Canonical did:mesh identifier, or null when looking up a name." },
      }
      : name === "mesh_await" ? { senders: { type: "array", maxItems: 16, uniqueItems: true, items: target },
        timeout_seconds: { type: "number", exclusiveMinimum: 0, maximum: 300, default: 300 } } : {},
    required: name === "mesh_send" ? ["to_agent", "content"] : [],
  };
}

export function peerArgumentError(name: PeerToolName, args: unknown): string | undefined {
  const keys = name === "mesh_send" ? ["to_agent", "content"] : name === "discover" ? ["name", "did"]
    : name === "mesh_await" ? ["senders", "timeout_seconds"] : [];
  if (!record(args) || Object.keys(args).some(key => !keys.includes(key))) return "Peer call requires only declared object arguments";
  if (name === "mesh_send" && (!peer(args.to_agent) || typeof args.content !== "string"
    || Buffer.byteLength(args.content) > 65536 || !ordinaryPeerMessage(args.content))) return "Peer send requires a valid peer and at most 65536 UTF-8 bytes of ordinary text, not protocol envelopes";
  if (name === "discover" && ((args.name != null) === (args.did != null)
    || (args.name != null && !peer(args.name)) || (args.did != null && (!peer(args.did) || !did(args.did))))) return "Discover requires exactly one valid name or canonical DID; omit the unused field or set it to null";
  if (name === "mesh_await") {
    const senders = args.senders ?? [];
    const timeout = args.timeout_seconds ?? 300;
    if (!Array.isArray(senders) || senders.length > 16 || !senders.every(peer) || new Set(senders).size !== senders.length
      || typeof timeout !== "number" || !Number.isFinite(timeout) || timeout <= 0 || timeout > 300) return "Peer wait requires at most 16 unique peers and a timeout above zero and at most 300 seconds";
  }
  return undefined;
}

export interface PeerInboxEntry {
  from_amid: string; from_agent: string; content: unknown; timestamp: string; id: string; message_type?: string; read_at?: string;
}
export interface MissionPeerDependencies {
  client: { supportsScopedOperations?: boolean; send(to: string, payload: unknown, operation: PeerOperation): Promise<unknown>; isPlaintextPeer(to: string): boolean };
  sandboxName: string;
  registry: IMeshRegistry;
  inbox: PeerInboxEntry[];
  partition: WeakSet<object>;
  assertCurrent(): void;
  waitForInbox(timeoutMs: number, signal: AbortSignal): Promise<boolean>;
  markRead(ids: string[]): void;
}
export type MissionPeers = (name: PeerToolName, args: Record<string, unknown>, operation: PeerOperation) => Promise<unknown>;

/** Adapters over the singleton's existing registry, encrypted send and inbox; no second receiver. */
export function createMissionPeers(deps: MissionPeerDependencies): MissionPeers {
  return async (name, args, invocation) => {
    const error = peerArgumentError(name, args);
    if (error) throw new Error(error);
    if (!Number.isFinite(invocation.deadline)) throw new Error("Native peer invocation requires a finite deadline");
    const abort = new AbortController();
    const duration = name === "mesh_await" ? Number(args.timeout_seconds ?? 300) * 1000 : 30_000;
    const deadline = Math.min(invocation.deadline, Date.now() + duration);
    const signal = AbortSignal.any([invocation.signal, abort.signal]);
    const operation: PeerOperation = { signal, deadline, assertCurrent: () => {
      signal.throwIfAborted(); invocation.assertCurrent(); deps.assertCurrent();
      if (Date.now() >= deadline) throw new Error("Native peer deadline exceeded");
    } };
    const timer = setTimeout(() => abort.abort(new Error("Native peer deadline exceeded")), Math.max(0, deadline - Date.now()));
    const bounded = (value: unknown) => {
      if (Buffer.byteLength(JSON.stringify(value)) > RESULT_BYTES) throw new Error("Native peer result exceeds 160 KiB");
      return value;
    };
    const discover = async (target: string): Promise<RegistryEntry[]> => {
      operation.assertCurrent();
      const options = { operation, timeoutMs: Math.min(5000, deadline - Date.now()), limit: 16 };
      const records = did(target) ? [await deps.registry.lookup(target, options)].filter((r): r is RegistryEntry => !!r)
        : await deps.registry.search(target, options);
      operation.assertCurrent();
      return records.filter(row => peer(row.amid) && did(row.amid) && (did(target) ? row.amid === target : row.display_name === target)
        && row.status !== "offline" && typeof row.last_seen === "string"
        && Date.parse(row.last_seen) <= Date.now() + 5000 && Date.parse(row.last_seen) >= Date.now() - 90_000);
    };
    const resolve = async (target: string): Promise<string> => {
      if (did(target)) return target;
      const peers = await discover(target);
      const ids = [...new Set(peers.map(row => row.amid))];
      if (ids.length !== 1) throw new Error("Peer name has no unique fresh registration; no send or retry performed");
      return ids[0];
    };
    try {
      operation.assertCurrent();
      if (deps.client.supportsScopedOperations !== true) throw new Error("Native peers require an operation-scoped mesh transport; rebuild the shared package before use");
      if (name === "discover") return bounded({ peers: await discover(String(args.name ?? args.did)) });
      if (name === "mesh_send") {
        const target = await resolve(args.to_agent as string);
        operation.assertCurrent();
        if (deps.client.isPlaintextPeer(target)) throw new Error("Native peer messages require encryption");
        await deps.client.send(target, { type: "message", content: args.content, from_agent: deps.sandboxName, timestamp: new Date().toISOString() }, operation);
        operation.assertCurrent();
        return { sent: true, to: target, delivery_confirmed: false };
      }
      const requested = name === "mesh_await" ? args.senders as string[] | undefined ?? [] : [];
      const senders = new Set(await Promise.all(requested.map(resolve)));
      const messages: PeerInboxEntry[] = [];
      const selected = new Set<PeerInboxEntry>();
      const received = new Set<string>();
      let truncated = false;
      let completed: boolean;
      const timedOut = () => {
        invocation.signal.throwIfAborted(); invocation.assertCurrent(); deps.assertCurrent();
        if (Date.now() >= invocation.deadline) throw new Error("Native peer invocation deadline exceeded");
        return { messages: [], completed: false, truncated };
      };
      for (;;) {
        if (Date.now() >= deadline) return timedOut();
        operation.assertCurrent();
        for (const entry of deps.inbox) {
          if (entry.read_at || selected.has(entry) || !deps.partition.has(entry)
            || !ordinaryPeerMessage({ type: entry.message_type, content: entry.content })
            || (name === "mesh_await" && (senders.size ? !senders.has(entry.from_amid) || received.has(entry.from_amid) : messages.length > 0))) continue;
          if (Buffer.byteLength(JSON.stringify({ messages: [...messages, entry], completed: false, truncated: false })) > RESULT_BYTES) {
            truncated = true; continue;
          }
          selected.add(entry); messages.push(entry); received.add(entry.from_amid);
        }
        completed = name === "mesh_inbox" || (senders.size > 0 ? [...senders].every(sender => received.has(sender)) : messages.length > 0);
        if (completed || truncated) break;
        // Timeout and abort leave even partially collected messages queued for a later call.
        try {
          if (!await deps.waitForInbox(Math.max(1, deadline - Date.now()), signal)) return timedOut();
        } catch (error) {
          if (Date.now() >= deadline) return timedOut();
          throw error;
        }
      }
      const result = bounded({ messages, completed, truncated });
      operation.assertCurrent();
      // Other native calls can consume while this call waits. Never return their messages twice.
      if (messages.some(message => !deps.inbox.includes(message) || message.read_at)) throw new Error("Peer inbox selection changed while waiting");
      for (let index = deps.inbox.length - 1; index >= 0; index--) {
        if (selected.has(deps.inbox[index])) deps.inbox.splice(index, 1);
      }
      deps.markRead(messages.map(message => message.id));
      return result;
    } finally {
      abort.abort(); clearTimeout(timer);
    }
  };
}
