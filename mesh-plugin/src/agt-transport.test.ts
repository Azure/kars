import { describe, it, expect, vi } from "vitest";
import { AgtTransport } from "./agt-transport.js";
import { generateIdentity } from "./identity.js";

describe("AgtTransport", () => {
  it("constructs without error", () => {
    const identity = generateIdentity();
    const transport = new AgtTransport({
      relayUrl: "wss://relay.example.com",
      registryUrl: "https://registry.example.com/v1",
      identity,
    });
    expect(transport).toBeDefined();
    expect(transport.isConnected).toBe(false);
    expect(transport.agentId).toBe(identity.agentId);
  });

  it("tracks plaintext peers", () => {
    const identity = generateIdentity();
    const transport = new AgtTransport({
      relayUrl: "wss://relay.example.com",
      registryUrl: "https://registry.example.com/v1",
      identity,
      plaintextPeers: ["peer-1"],
    });
    expect(transport.isPlaintextPeer("peer-1")).toBe(true);
    expect(transport.isPlaintextPeer("peer-2")).toBe(false);

    transport.addPlaintextPeer("peer-2");
    expect(transport.isPlaintextPeer("peer-2")).toBe(true);
    expect(transport.getPlaintextPeers()).toContain("peer-1");
    expect(transport.getPlaintextPeers()).toContain("peer-2");

    transport.removePlaintextPeer("peer-1");
    expect(transport.isPlaintextPeer("peer-1")).toBe(false);
  });

  it("registers message handlers", () => {
    const identity = generateIdentity();
    const transport = new AgtTransport({
      relayUrl: "wss://relay.example.com",
      registryUrl: "https://registry.example.com/v1",
      identity,
    });
    const handler = vi.fn();
    transport.onMessage(handler);
    // Handler is registered but won't fire without a connection
    expect(handler).not.toHaveBeenCalled();
  });

  it("registers knock handlers", () => {
    const identity = generateIdentity();
    const transport = new AgtTransport({
      relayUrl: "wss://relay.example.com",
      registryUrl: "https://registry.example.com/v1",
      identity,
    });
    const handler = vi.fn(async () => ({ accept: true }));
    transport.onKnock(handler);
    expect(handler).not.toHaveBeenCalled();
  });

  it("send throws when not connected", async () => {
    const identity = generateIdentity();
    const transport = new AgtTransport({
      relayUrl: "wss://relay.example.com",
      registryUrl: "https://registry.example.com/v1",
      identity,
    });
    await expect(transport.send("peer", { hello: true })).rejects.toThrow("Not connected");
  });

  it("disconnect when not connected is a no-op", async () => {
    const identity = generateIdentity();
    const transport = new AgtTransport({
      relayUrl: "wss://relay.example.com",
      registryUrl: "https://registry.example.com/v1",
      identity,
    });
    await expect(transport.disconnect()).resolves.toBeUndefined();
    expect(transport.isConnected).toBe(false);
  });

  it("discover returns empty array on network error", async () => {
    const identity = generateIdentity();
    const transport = new AgtTransport({
      relayUrl: "wss://relay.example.com",
      registryUrl: "https://registry.invalid.example.com",
      identity,
    });
    const results = await transport.discover({ capabilities: ["test"] });
    expect(results).toEqual([]);
  });

  it("search returns empty array on network error", async () => {
    const identity = generateIdentity();
    const transport = new AgtTransport({
      relayUrl: "wss://relay.example.com",
      registryUrl: "https://registry.invalid.example.com",
      identity,
    });
    const results = await transport.search("test-cap");
    expect(results).toEqual([]);
  });
});
