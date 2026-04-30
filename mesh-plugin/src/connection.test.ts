// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect, vi } from "vitest";
import { MeshConnection } from "./connection.js";
import { generateIdentity } from "./identity.js";

/**
 * These tests exercise MeshConnection's app-layer logic (inbox, waiters,
 * chunk reassembly, sendWithAck, pingPeer) without touching the real SDK.
 * The `@agentmesh/sdk` client is replaced with a stub that just captures
 * `send()` calls and lets us synthesise inbound messages via the registered
 * onMessage handler.
 *
 * WebSocket, auth, prekey upload, Signal E2E, and reconnect are owned by
 * the SDK and covered by its own test suite.
 */

function makeStubClient() {
  let messageHandler: ((from: string, content: unknown) => void) | null = null;
  const plaintextPeers = new Set<string>();
  return {
    _isConnected: false,
    messageHandler() { return messageHandler; },
    onMessage(h: any) { messageHandler = h; },
    onKnock(_h: any) {},
    addPlaintextPeer(a: string) { plaintextPeers.add(a); },
    removePlaintextPeer(a: string) { plaintextPeers.delete(a); },
    getPlaintextPeers() { return [...plaintextPeers]; },
    async connect(_opts?: any) { this._isConnected = true; },
    async disconnect() { this._isConnected = false; },
    get isConnected() { return this._isConnected; },
    send: vi.fn(async (_to: string, _msg: any) => { /* captured */ }),
  };
}

/**
 * Build a MeshConnection with its SDK client swapped out, bypassing the
 * real `doConnect()`. Returns the connection and the stub client so tests
 * can inspect/drive it.
 */
async function makeConnection(opts?: { maxInboxSize?: number }) {
  const identity = await generateIdentity();
  const conn = new MeshConnection({
    relayUrl: "wss://relay.example.com/v1/connect",
    registryUrl: "https://registry.example.com/v1",
    identity,
    maxInboxSize: opts?.maxInboxSize,
  });
  const stub = makeStubClient();
  (conn as any).client = stub;
  stub._isConnected = true;
  // Wire the handler the same way connect() would
  stub.onMessage((from: string, content: unknown) =>
    (conn as any).onClientMessage(from, content),
  );
  return { conn, stub, identity };
}

describe("MeshConnection", () => {
  it("initializes with correct state", async () => {
    const identity = await generateIdentity();
    const conn = new MeshConnection({
      relayUrl: "wss://relay.example.com/v1/connect",
      registryUrl: "https://registry.example.com/v1",
      identity,
    });
    expect(conn.isConnected).toBe(false);
    expect(conn.amid).toBe(identity.amid);
  });

  it("getInbox returns empty array initially", async () => {
    const identity = await generateIdentity();
    const conn = new MeshConnection({
      relayUrl: "wss://relay.example.com/v1/connect",
      registryUrl: "https://registry.example.com/v1",
      identity,
    });
    expect(conn.getInbox()).toEqual([]);
    expect(conn.drainInbox()).toEqual([]);
  });

  it("rejects send when not connected", async () => {
    const identity = await generateIdentity();
    const conn = new MeshConnection({
      relayUrl: "wss://relay.example.com/v1/connect",
      registryUrl: "https://registry.example.com/v1",
      identity,
    });
    await expect(conn.send("target", { hello: true })).rejects.toThrow(
      "Not connected",
    );
  });

  it("waitForMessage times out when no message arrives", async () => {
    const identity = await generateIdentity();
    const conn = new MeshConnection({
      relayUrl: "wss://relay.example.com/v1/connect",
      registryUrl: "https://registry.example.com/v1",
      identity,
    });
    await expect(conn.waitForMessage(() => null, 100)).rejects.toThrow(
      "Timed out",
    );
  });

  // ── connect() is single-flight — concurrent callers share one attempt ──
  it("connect() is single-flight", async () => {
    const identity = await generateIdentity();
    const conn = new MeshConnection({
      relayUrl: "wss://relay.example.com/v1/connect",
      registryUrl: "https://registry.example.com/v1",
      identity,
    });
    // Stub doConnect so we don't actually touch the network
    let resolveConnect!: () => void;
    const pending = new Promise<void>((r) => { resolveConnect = r; });
    const spy = vi.spyOn(conn as any, "doConnect").mockImplementation(() => pending);

    const p1 = conn.connect();
    const p2 = conn.connect();
    expect(spy).toHaveBeenCalledTimes(1); // only one underlying attempt
    resolveConnect();
    await Promise.all([p1, p2]);
    spy.mockRestore();
  });

  // ── consumeInbox removes only matching messages ──────────────────────
  it("consumeInbox claims matching messages and preserves others", async () => {
    const { conn } = await makeConnection();
    (conn as any).pushInbox({
      from: "peer-a",
      content: { type: "offload_progress", request_id: "req-1" },
      timestamp: new Date().toISOString(),
    });
    (conn as any).pushInbox({
      from: "peer-b",
      content: { type: "chat", content: "hello" },
      timestamp: new Date().toISOString(),
    });
    (conn as any).pushInbox({
      from: "peer-a",
      content: { type: "offload_done", request_id: "req-1" },
      timestamp: new Date().toISOString(),
    });

    const claimed = conn.consumeInbox((m) => {
      const c = m.content as Record<string, unknown>;
      return c?.request_id === "req-1";
    });
    expect(claimed).toHaveLength(2);
    expect(conn.getInbox()).toHaveLength(1);
    expect((conn.getInbox()[0].content as any).type).toBe("chat");
  });

  // ── maxInboxSize caps inbox growth ───────────────────────────────────
  it("inbox is capped at maxInboxSize (FIFO drop)", async () => {
    const { conn } = await makeConnection({ maxInboxSize: 3 });
    for (let i = 0; i < 10; i++) {
      (conn as any).pushInbox({
        from: "peer",
        content: { i },
        timestamp: new Date().toISOString(),
      });
    }
    const inbox = conn.getInbox();
    expect(inbox).toHaveLength(3);
    expect((inbox[0].content as any).i).toBe(7);
    expect((inbox[2].content as any).i).toBe(9);
  });

  it("default maxInboxSize is 5000", async () => {
    const identity = await generateIdentity();
    const conn = new MeshConnection({
      relayUrl: "wss://relay.example.com/v1/connect",
      registryUrl: "https://registry.example.com/v1",
      identity,
    });
    expect((conn as any).maxInboxSize).toBe(5000);
  });

  // ── Concurrent waiters match independently ────────────────────────────
  it("supports multiple concurrent waiters", async () => {
    const { conn, stub } = await makeConnection();
    const p1 = conn.waitForMessage<string>(
      (c) => (c as any)?.tag === "a" ? "got-a" : null,
      1000,
    );
    const p2 = conn.waitForMessage<string>(
      (c) => (c as any)?.tag === "b" ? "got-b" : null,
      1000,
    );
    stub.messageHandler()!("peer", { tag: "a" });
    stub.messageHandler()!("peer", { tag: "b" });

    await expect(p1).resolves.toBe("got-a");
    await expect(p2).resolves.toBe("got-b");
  });

  it("waitForMessage consumes matched messages by default", async () => {
    const { conn, stub } = await makeConnection();
    const p = conn.waitForMessage<string>(
      (c) => (c as any)?.tag === "x" ? "x" : null,
      1000,
    );
    stub.messageHandler()!("peer", { tag: "x" });
    await expect(p).resolves.toBe("x");
    expect(conn.getInbox()).toHaveLength(0);
  });

  it("waitForMessage with consume:false preserves the message", async () => {
    const { conn, stub } = await makeConnection();
    const p = conn.waitForMessage<string>(
      (c) => (c as any)?.tag === "x" ? "peeked" : null,
      1000,
      { consume: false },
    );
    stub.messageHandler()!("peer", { tag: "x" });
    await expect(p).resolves.toBe("peeked");
    expect(conn.getInbox()).toHaveLength(1);
  });

  // ── pingPeer matches pong by nonce ───────────────────────────────────
  it("pingPeer matches pong by nonce", async () => {
    const { conn, stub } = await makeConnection();
    let capturedNonce: string | null = null;
    stub.send.mockImplementation(async (_to: string, payload: any) => {
      capturedNonce = payload.nonce;
      setImmediate(() => {
        stub.messageHandler()!("sandbox-amid", {
          type: "mesh:pong",
          nonce: capturedNonce,
          from_agent: "sandbox-1",
        });
      });
    });

    const result = await conn.pingPeer("sandbox-amid", { timeoutMs: 1000, retries: 0 });
    expect(result.rttMs).toBeGreaterThanOrEqual(0);
    expect((result.pong as any).nonce).toBe(capturedNonce);
  });

  // ── sendWithAck retries on ack timeout ───────────────────────────────
  it("sendWithAck retries then eventually matches ack", async () => {
    const { conn, stub } = await makeConnection();
    let attempts = 0;
    stub.send.mockImplementation(async (_to: string, _payload: any) => {
      attempts++;
      if (attempts >= 3) {
        setImmediate(() => {
          stub.messageHandler()!("peer", { type: "task_received", request_id: "r1" });
        });
      }
    });

    const ack = await conn.sendWithAck(
      "peer",
      { type: "offload_task", request_id: "r1" },
      (c, f) =>
        f === "peer" && (c as any)?.type === "task_received" ? c : null,
      { timeoutMs: 80, retries: 3, retryDelayMs: 5 },
    );
    expect((ack as any).request_id).toBe("r1");
    expect(attempts).toBe(3);
  });

  // ── Chunk-reassembly: manifest + chunks reassemble into final message ──
  it("chunk reassembly reconstructs the original payload", async () => {
    const { conn, stub } = await makeConnection();
    const original = JSON.stringify({ type: "big_thing", data: "x".repeat(100) });
    const crypto = await import("node:crypto");
    const sha = (s: string) => crypto.createHash("sha256").update(s).digest("hex");
    const chunks = [original.slice(0, 50), original.slice(50)];
    const chunkHashes = chunks.map(sha);
    const manifestHash = sha(chunkHashes.join(":"));
    const transferId = "tid-1";

    stub.messageHandler()!("peer", {
      type: "mesh:transfer_manifest",
      transfer_id: transferId,
      original_type: "big_thing",
      total_chunks: 2,
      total_bytes: original.length,
      chunk_hashes: chunkHashes,
      manifest_hash: manifestHash,
    });
    stub.messageHandler()!("peer", {
      type: "mesh:transfer_chunk",
      transfer_id: transferId,
      chunk_index: 0,
      total_chunks: 2,
      data: chunks[0],
      hash: chunkHashes[0],
    });
    // Inbox should still be empty — only the manifest + chunk-0 so far
    expect(conn.getInbox()).toHaveLength(0);

    stub.messageHandler()!("peer", {
      type: "mesh:transfer_chunk",
      transfer_id: transferId,
      chunk_index: 1,
      total_chunks: 2,
      data: chunks[1],
      hash: chunkHashes[1],
    });

    const inbox = conn.getInbox();
    expect(inbox).toHaveLength(1);
    expect((inbox[0].content as any).type).toBe("big_thing");
  });

  // ── addPlaintextPeer plumbs through to the SDK client ────────────────
  it("addPlaintextPeer forwards to SDK client", async () => {
    const { conn, stub } = await makeConnection();
    conn.addPlaintextPeer("controller-amid");
    expect(stub.getPlaintextPeers()).toContain("controller-amid");
    conn.removePlaintextPeer("controller-amid");
    expect(stub.getPlaintextPeers()).not.toContain("controller-amid");
  });

  // ── send() fast path calls the SDK once ──────────────────────────────
  it("send small payload passes object through to SDK.send", async () => {
    const { conn, stub } = await makeConnection();
    await conn.send("peer-amid", { type: "hello", x: 1 });
    expect(stub.send).toHaveBeenCalledTimes(1);
    expect(stub.send.mock.calls[0][0]).toBe("peer-amid");
    expect(stub.send.mock.calls[0][1]).toEqual({ type: "hello", x: 1 });
  });
});
