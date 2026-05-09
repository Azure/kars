// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * LocalInbox — provider-agnostic inbox/waiter/diagnostics machinery.
 *
 * Phase 5 of the agentmesh provider swap extracted the inbox surface from
 * MeshConnection so AgtTransport can compose the same semantics on top of
 * the upstream `@microsoft/agent-governance-sdk` MeshClient (which only
 * exposes an `onMessage` callback and has no inbox of its own).
 *
 * Both transports push every decrypted message into a `LocalInbox` via
 * {@link LocalInbox.push}; callers consume via the same getInbox /
 * consumeInbox / waitForMessage / waitForInbox surface they used before.
 *
 * No I/O. No external deps beyond `node:crypto` for the message id. Safe
 * to instantiate per-connection.
 */

import * as crypto from "node:crypto";

export interface InboxMessage {
  id: string;
  from: string;
  content: unknown;
  timestamp: string;
  read_at?: string;
}

export interface InboxDiagnostics {
  build_hash: string;
  received_total: number;
  consumed_by_waiter: number;
  consumed_by_predicate: number;
  fifo_dropped: number;
  read_total: number;
  last_received_at: string | null;
  last_read_at: string | null;
}

interface Waiter {
  predicate: (content: unknown, from: string) => unknown;
  resolve: (v: unknown) => void;
  consume: boolean;
}

export class LocalInbox {
  private readonly buildHash: string;
  private readonly maxSize: number;
  private inbox: InboxMessage[] = [];
  private waiters = new Set<Waiter>();
  private inboxWakers = new Set<() => void>();
  private stats = {
    received_total: 0,
    consumed_by_waiter: 0,
    consumed_by_predicate: 0,
    fifo_dropped: 0,
    read_total: 0,
    last_received_at: null as string | null,
    last_read_at: null as string | null,
  };

  constructor(opts: { buildHash: string; maxSize?: number }) {
    this.buildHash = opts.buildHash;
    this.maxSize = opts.maxSize ?? 5000;
  }

  /**
   * Deliver a freshly received message. First offered to active waiters; if
   * none claim it, pushed into the FIFO inbox and broadcast to wakers.
   * Returns true iff a waiter consumed the message (caller may then skip
   * any side-effect storage).
   */
  deliver(from: string, content: unknown): boolean {
    if (this.deliverToWaiters(from, content)) return true;
    const ts = new Date().toISOString();
    this.push({
      id: `mesh-${Date.now().toString(36)}-${crypto.randomUUID().slice(0, 8)}`,
      from,
      content,
      timestamp: ts,
    });
    return false;
  }

  private deliverToWaiters(from: string, content: unknown): boolean {
    if (this.waiters.size === 0) return false;
    for (const waiter of [...this.waiters]) {
      let result: unknown;
      try {
        result = waiter.predicate(content, from);
      } catch (err) {
        this.waiters.delete(waiter);
        waiter.resolve(err);
        if (waiter.consume) this.stats.consumed_by_waiter += 1;
        return waiter.consume;
      }
      if (result !== null && result !== undefined) {
        this.waiters.delete(waiter);
        waiter.resolve(result);
        if (waiter.consume) {
          this.stats.consumed_by_waiter += 1;
          return true;
        }
      }
    }
    return false;
  }

  private push(msg: InboxMessage): void {
    this.inbox.push(msg);
    this.stats.received_total += 1;
    this.stats.last_received_at = msg.timestamp;
    while (this.inbox.length > this.maxSize) {
      this.inbox.shift();
      this.stats.fifo_dropped += 1;
    }
    if (this.inboxWakers.size > 0) {
      const wakers = Array.from(this.inboxWakers);
      this.inboxWakers.clear();
      for (const w of wakers) {
        try { w(); } catch { /* swallow */ }
      }
    }
  }

  getInbox(limit?: number): InboxMessage[] {
    return limit ? this.inbox.slice(-limit) : [...this.inbox];
  }

  markRead(ids: string[]): number {
    if (ids.length === 0) return 0;
    const idSet = new Set(ids);
    const now = new Date().toISOString();
    let count = 0;
    for (const m of this.inbox) {
      if (idSet.has(m.id) && !m.read_at) {
        m.read_at = now;
        count += 1;
      }
    }
    if (count > 0) {
      this.stats.read_total += count;
      this.stats.last_read_at = now;
    }
    return count;
  }

  getUnreadCount(): number {
    let n = 0;
    for (const m of this.inbox) if (!m.read_at) n += 1;
    return n;
  }

  drainInbox(): InboxMessage[] {
    const msgs = [...this.inbox];
    this.stats.consumed_by_predicate += msgs.length;
    this.inbox = [];
    return msgs;
  }

  consumeInbox(predicate: (msg: InboxMessage) => boolean): InboxMessage[] {
    const claimed: InboxMessage[] = [];
    const kept: InboxMessage[] = [];
    for (const m of this.inbox) {
      if (predicate(m)) claimed.push(m);
      else kept.push(m);
    }
    if (claimed.length > 0) {
      this.inbox = kept;
      this.stats.consumed_by_predicate += claimed.length;
    }
    return claimed;
  }

  async waitForMessage<T>(
    predicate: (content: unknown, from: string) => T | null,
    timeoutMs = 15_000,
    opts: { consume?: boolean } = {},
  ): Promise<T> {
    const consume = opts.consume !== false;
    for (let i = 0; i < this.inbox.length; i++) {
      const msg = this.inbox[i];
      const result = predicate(msg.content, msg.from);
      if (result !== null && result !== undefined) {
        if (consume) {
          this.inbox.splice(i, 1);
          this.stats.consumed_by_waiter += 1;
        }
        return result;
      }
    }
    return new Promise<T>((resolve, reject) => {
      const waiter: Waiter = {
        predicate: predicate as (content: unknown, from: string) => unknown,
        consume,
        resolve: (v: unknown) => {
          clearTimeout(timer);
          if (v instanceof Error) reject(v);
          else resolve(v as T);
        },
      };
      const timer = setTimeout(() => {
        this.waiters.delete(waiter);
        reject(new Error(`Timed out waiting for message (${timeoutMs}ms)`));
      }, timeoutMs);
      this.waiters.add(waiter);
    });
  }

  waitForInbox(timeoutMs: number): Promise<boolean> {
    return new Promise<boolean>((resolve) => {
      let done = false;
      const waker = () => {
        if (done) return;
        done = true;
        clearTimeout(timer);
        this.inboxWakers.delete(waker);
        resolve(true);
      };
      const timer = setTimeout(() => {
        if (done) return;
        done = true;
        this.inboxWakers.delete(waker);
        resolve(false);
      }, Math.max(1, timeoutMs));
      if (typeof (timer as unknown as { unref?: () => void }).unref === "function") {
        (timer as unknown as { unref: () => void }).unref();
      }
      this.inboxWakers.add(waker);
    });
  }

  getDiagnostics(): InboxDiagnostics {
    return {
      build_hash: this.buildHash,
      received_total: this.stats.received_total,
      consumed_by_waiter: this.stats.consumed_by_waiter,
      consumed_by_predicate: this.stats.consumed_by_predicate,
      fifo_dropped: this.stats.fifo_dropped,
      read_total: this.stats.read_total,
      last_received_at: this.stats.last_received_at,
      last_read_at: this.stats.last_read_at,
    };
  }

  /** Test-only escape hatch: count of pending waiters. */
  get waiterCount(): number {
    return this.waiters.size;
  }
}
