// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import { createMissionPeers, ordinaryPeerMessage, peerArgumentError, peerSchema, type MissionPeerDependencies, type PeerInboxEntry, type PeerToolName } from "./mission-peer.js";

const did = `did:mesh:${"a".repeat(32)}`;
const other = `did:mesh:${"b".repeat(32)}`;
function fixture() {
  let current = true;
  const abort = new AbortController();
  const deps: MissionPeerDependencies = {
    client: { supportsScopedOperations: true, send: vi.fn(async () => {}), isPlaintextPeer: () => false },
    sandboxName: "writer", inbox: [], partition: new WeakSet(),
    registry: { provider: "agt", search: vi.fn(async () => [{ amid: did, display_name: "reviewer", last_seen: new Date().toISOString() }]), lookup: vi.fn(async () => null) },
    assertCurrent: () => { if (!current) throw new Error("stale assignment"); },
    waitForInbox: vi.fn(async () => false), markRead: vi.fn(),
  };
  const run = createMissionPeers(deps);
  const entry = (content: unknown = "hello", sender = did, owned = true) => {
    const value: PeerInboxEntry = { id: String(deps.inbox.length), from_amid: sender, from_agent: "untrusted-name", content, timestamp: new Date().toISOString(), message_type: "message" };
    deps.inbox.push(value); if (owned) deps.partition.add(value); return value;
  };
  const call = (name: PeerToolName, args: Record<string, unknown> = {}) => run(name, args, { signal: abort.signal, deadline: Date.now() + 10000, assertCurrent: deps.assertCurrent });
  return { deps, call, entry, abort, revoke: () => { current = false; } };
}
afterEach(() => vi.useRealTimers());

describe("bounded native peer adapters", () => {
  it.each([
    ["mesh_send", { to_agent: "bad name", content: "ok" }],
    ["mesh_send", { to_agent: did, content: "é".repeat(32769) }],
    ["mesh_send", { to_agent: did, content: '{"type":"mission:assign"}' }],
    ["mesh_send", { to_agent: did, content: JSON.stringify({ type: "message", content: JSON.stringify({ type: "mission:reply" }) }) }],
    ["mesh_send", { to_agent: did, content: JSON.stringify(JSON.stringify({ type: "task_request" })) }],
    ["discover", {}], ["discover", { name: "reviewer", did }], ["discover", { did: "did:mesh:bad" }],
    ["discover", { name: null, did: null }], ["discover", { name: "reviewer", did: "/" }],
    ["discover", { name: "reviewer", did: "" }], ["discover", { name: null, did: "did:mesh:bad" }],
    ["mesh_await", { senders: Array(17).fill(did) }], ["mesh_await", { senders: [did, did] }],
    ...[NaN, Infinity, 0, -1, 301].map(timeout_seconds => ["mesh_await", { timeout_seconds }]),
    ["mesh_inbox", { run: "another" }],
  ] as [PeerToolName, Record<string, unknown>][]) ("rejects invalid %s arguments before effects %#", async (name, args) => {
    const f = fixture();
    expect(peerArgumentError(name, args)).toBeTypeOf("string");
    await expect(f.call(name, args)).rejects.toThrow();
    expect(f.deps.client.send).not.toHaveBeenCalled(); expect(f.deps.registry.search).not.toHaveBeenCalled();
  });
  it.each([{ name: "reviewer" }, { name: "reviewer", did: null }, { did }, { name: null, did }])(
    "discovers exactly one target with omitted or strict-schema null alternatives %j", async args => {
      const f = fixture();
      vi.mocked(f.deps.registry.lookup).mockResolvedValue({ amid: did, display_name: "reviewer", last_seen: new Date().toISOString() });
      expect(peerArgumentError("discover", args)).toBeUndefined();
      await expect(f.call("discover", args)).resolves.toMatchObject({ peers: [{ amid: did }] });
      expect(f.deps.client.send).not.toHaveBeenCalled();
      expect(args.name ? f.deps.registry.search : f.deps.registry.lookup).toHaveBeenCalledOnce();
    });
  it("keeps discover representable when strict providers require every property", () => {
    const schema = peerSchema("discover");
    expect(schema).not.toHaveProperty("oneOf");
    expect(schema.properties).toMatchObject({
      name: { type: ["string", "null"] }, did: { type: ["string", "null"] },
    });
    expect(schema.additionalProperties).toBe(false);
  });
  it("allows ordinary text/JSON but excludes nested protocol and excessive recursion", () => {
    expect(ordinaryPeerMessage({ type: "message", content: '{"findings":["safe"]}' })).toBe(true);
    expect(ordinaryPeerMessage({ content: [{ type: "mesh:transfer_chunk" }] })).toBe(false);
    let value: unknown = { type: "mission:assign" };
    for (let i = 0; i < 18; i++) value = [value];
    expect(ordinaryPeerMessage(value)).toBe(false);
  });
  it("sends once through the scoped singleton with bounded authority, not a delivery claim", async () => {
    const f = fixture();
    await expect(f.call("mesh_send", { to_agent: "reviewer", content: "é".repeat(32768) })).resolves.toMatchObject({ sent: true, to: did, delivery_confirmed: false });
    expect(f.deps.client.send).toHaveBeenCalledOnce();
    const [target, body, operation] = vi.mocked(f.deps.client.send).mock.calls[0];
    expect(target).toBe(did); expect(body).toMatchObject({ type: "message", from_agent: "writer" });
    expect(operation.deadline).toBeLessThanOrEqual(Date.now() + 30000);
    expect(operation.signal.aborted).toBe(true);
    expect(() => operation.assertCurrent()).toThrow();
  });
  it.each(["stale", "abort", "old-package", "plaintext", "ambiguous", "offline"])("fails closed for %s without sending", async cause => {
    const f = fixture();
    if (cause === "stale") f.revoke();
    if (cause === "abort") f.abort.abort();
    if (cause === "old-package") delete f.deps.client.supportsScopedOperations;
    if (cause === "plaintext") f.deps.client.isPlaintextPeer = () => true;
    if (cause === "ambiguous") vi.mocked(f.deps.registry.search).mockResolvedValue([did, other].map(amid => ({ amid, display_name: "reviewer", last_seen: new Date().toISOString() })));
    if (cause === "offline") vi.mocked(f.deps.registry.search).mockResolvedValue([{ amid: did, display_name: "reviewer", last_seen: new Date(Date.now() - 91000).toISOString() }]);
    await expect(f.call("mesh_send", { to_agent: "reviewer", content: "hello" })).rejects.toThrow();
    expect(f.deps.client.send).not.toHaveBeenCalled();
  });
  it("rejects an unregistered planned peer before encryption without retrying", async () => {
    const f = fixture();
    vi.mocked(f.deps.registry.search).mockResolvedValue([]);
    const target = `rp-${"a".repeat(20)}-r1-p0`;
    await expect(f.call("mesh_send", { to_agent: target, content: "Please review the evidence" }))
      .rejects.toThrow("Peer name has no unique fresh registration; no send or retry performed");
    expect(f.deps.registry.search).toHaveBeenCalledOnce();
    expect(f.deps.registry.search).toHaveBeenCalledWith(target, expect.any(Object));
    expect(f.deps.client.send).not.toHaveBeenCalled();
  });
  it("never retries a failed send, nor returns a stale discovery result", async () => {
    const f = fixture();
    vi.mocked(f.deps.client.send).mockRejectedValue(new Error("unconfirmed; must not replay"));
    await expect(f.call("mesh_send", { to_agent: did, content: "hello" })).rejects.toThrow("unconfirmed");
    expect(f.deps.client.send).toHaveBeenCalledOnce();
    vi.mocked(f.deps.registry.search).mockImplementation(async () => { f.revoke(); return []; });
    await expect(f.call("discover", { name: "reviewer" })).rejects.toThrow("stale");
  });
  it("consumes only unread ordinary entries in this assignment partition, once", async () => {
    const f = fixture();
    const old = f.entry("old run", did, false);
    const protocol = f.entry('{"type":"mission:assign"}');
    const read = f.entry("already read"); read.read_at = "now";
    const selected = f.entry();
    await expect(f.call("mesh_inbox")).resolves.toEqual({ messages: [selected], completed: true, truncated: false });
    expect(f.deps.inbox).toEqual([old, protocol, read]);
    expect(f.deps.markRead).toHaveBeenCalledWith([selected.id]);
    await expect(f.call("mesh_inbox")).resolves.toMatchObject({ messages: [] });
  });
  it("bounds complete serialized results and leaves overflow queued", async () => {
    const f = fixture();
    const entries = [f.entry("a".repeat(60000)), f.entry("b".repeat(60000)), f.entry("c".repeat(60000))];
    const result = await f.call("mesh_inbox") as { messages: PeerInboxEntry[]; truncated: boolean };
    expect(Buffer.byteLength(JSON.stringify(result))).toBeLessThanOrEqual(160 * 1024);
    expect(result.messages).toEqual(entries.slice(0, 2)); expect(result.truncated).toBe(true);
    expect(f.deps.inbox).toEqual(entries.slice(2));
  });
  it("waits by authenticated DID, not claimed name; returns ordinary payloads", async () => {
    const f = fixture(); const wrong = f.entry("wrong", other); wrong.from_agent = "reviewer";
    vi.mocked(f.deps.waitForInbox).mockImplementation(async () => { f.entry("answer"); return true; });
    const result = await f.call("mesh_await", { senders: ["reviewer"], timeout_seconds: 1 }) as any;
    expect(result.messages.map((m: PeerInboxEntry) => m.content)).toEqual(["answer"]);
    expect(result.completed).toBe(true); expect(f.deps.inbox).toEqual([wrong]);
  });
  it.each(["abort", "stale", "consumed"])("leaves held inbox data intact after %s during wait", async cause => {
    const f = fixture(); const held = f.entry();
    vi.mocked(f.deps.waitForInbox).mockImplementation(async () => {
      if (cause === "abort") f.abort.abort();
      if (cause === "stale") f.revoke();
      if (cause === "consumed") held.read_at = "another consumer";
      f.entry("second", other); return true;
    });
    await expect(f.call("mesh_await", { senders: [did, other] })).rejects.toThrow();
    expect(f.deps.inbox).toContain(held); expect(f.deps.markRead).not.toHaveBeenCalled();
  });
  it("times out without consuming partial selections and cleans invocation timers", async () => {
    vi.useFakeTimers(); const f = fixture(); const held = f.entry();
    vi.mocked(f.deps.waitForInbox).mockImplementation((_ms, signal) => new Promise((_resolve, reject) => signal.addEventListener("abort", () => reject(signal.reason), { once: true })));
    const pending = f.call("mesh_await", { senders: [did, other], timeout_seconds: 0.02 });
    await vi.advanceTimersByTimeAsync(21);
    await expect(pending).resolves.toEqual({ messages: [], completed: false, truncated: false });
    expect(f.deps.inbox).toEqual([held]); expect(f.deps.markRead).not.toHaveBeenCalled(); expect(vi.getTimerCount()).toBe(0);
  });
});
