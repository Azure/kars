import { describe, it, expect } from "vitest";
import { MeshConnection } from "./connection.js";
import { generateIdentity } from "./identity.js";

describe("MeshConnection", () => {
  it("initializes with correct state", () => {
    const identity = generateIdentity();
    const conn = new MeshConnection({
      relayUrl: "wss://relay.example.com/v1/connect",
      registryUrl: "https://registry.example.com/v1",
      identity,
    });
    expect(conn.isConnected).toBe(false);
    expect(conn.amid).toBe(identity.amid);
  });

  it("getInbox returns empty array initially", () => {
    const identity = generateIdentity();
    const conn = new MeshConnection({
      relayUrl: "wss://relay.example.com/v1/connect",
      registryUrl: "https://registry.example.com/v1",
      identity,
    });
    expect(conn.getInbox()).toEqual([]);
    expect(conn.drainInbox()).toEqual([]);
  });

  it("rejects send when not connected", async () => {
    const identity = generateIdentity();
    const conn = new MeshConnection({
      relayUrl: "wss://relay.example.com/v1/connect",
      registryUrl: "https://registry.example.com/v1",
      identity,
    });
    await expect(conn.send("target", { hello: true })).rejects.toThrow(
      "Not connected"
    );
  });

  it("connect rejects on invalid URL", async () => {
    const identity = generateIdentity();
    const conn = new MeshConnection({
      relayUrl: "wss://127.0.0.1:1/invalid",
      registryUrl: "https://127.0.0.1:1/v1",
      identity,
    });
    await expect(conn.connect()).rejects.toThrow();
  });

  it("waitForMessage times out when no message arrives", async () => {
    const identity = generateIdentity();
    const conn = new MeshConnection({
      relayUrl: "wss://relay.example.com/v1/connect",
      registryUrl: "https://registry.example.com/v1",
      identity,
    });
    await expect(
      conn.waitForMessage(() => null, 100)
    ).rejects.toThrow("Timed out");
  });
});
