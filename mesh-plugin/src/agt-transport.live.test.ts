// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Real pinned SDK, encrypted bidirectional delivery through operator-provided
 * AgentMesh services. Uses fresh test identities, never a running agent's keys.
 *
 * With the Core Helm release installed, forward relay 8765 to local 18083 and
 * registry 8080 to local 18082 in the dedicated test cluster, then run:
 *   KARS_LIVE_AGT=1 npm test -- src/agt-transport.live.test.ts
 * Override AGENTMESH_LIVE_RELAY_URL / AGENTMESH_LIVE_REGISTRY_URL if needed.
 * This anonymous local test is not AKS Entra or mission-completion evidence.
 */

import { describe, it, expect, beforeAll, afterAll } from "vitest";
import * as crypto from "node:crypto";
import { createMeshTransport } from "./transport-factory.js";
import type { IMeshTransport } from "./transport-interface.js";
import WebSocket from "ws";

const LIVE = !!process.env.KARS_LIVE_AGT;
const RELAY = process.env.AGENTMESH_LIVE_RELAY_URL || "http://localhost:18083";
const REGISTRY = process.env.AGENTMESH_LIVE_REGISTRY_URL || "http://localhost:18082";

function newKeypair() {
  // Generate a raw Ed25519 keypair via Node crypto and extract the 32-byte
  // seed (private) and 32-byte public key — the same shape AGT's
  // X3DHKeyManager and our IMeshIdentity expect.
  const { publicKey, privateKey } = crypto.generateKeyPairSync("ed25519");
  const pubDer = publicKey.export({ format: "der", type: "spki" });
  const privDer = privateKey.export({ format: "der", type: "pkcs8" });
  // SPKI for ed25519 ends with the 32-byte raw public key.
  const pub = new Uint8Array(pubDer.subarray(pubDer.length - 32));
  // PKCS#8 for ed25519 ends with the 32-byte raw private seed.
  const priv = new Uint8Array(privDer.subarray(privDer.length - 32));
  const fp = crypto.createHash("sha256").update(pub).digest("hex").slice(0, 32);
  return {
    signingPublicKey: pub,
    signTimestamp: (timestamp: string) => crypto.sign(null, Buffer.from(timestamp), privateKey).toString("base64url"),
    signingPrivateKey: priv,
    did: `did:mesh:${fp}`,
  };
}

describe.skipIf(!LIVE)("AGT live mesh round-trip", () => {
  let alice: IMeshTransport;
  let bob: IMeshTransport;
  const aliceKp = newKeypair();
  const bobKp = newKeypair();

  const frames: Array<Record<string, unknown>> = [];
  const wsFactory = (url: string) => {
    const socket = new WebSocket(url);
    const send = socket.send.bind(socket);
    socket.send = ((data: WebSocket.RawData, ...args: unknown[]) => {
      const frame = JSON.parse(data.toString()) as Record<string, unknown>;
      if (frame.type === "message") frames.push(frame);
      return Reflect.apply(send, socket, [data, ...args]);
    }) as typeof socket.send;
    return socket;
  };

  beforeAll(async () => {
    alice = await createMeshTransport({
      relayUrl: RELAY,
      registryUrl: REGISTRY,
      identity: {
        amid: aliceKp.did,
        did: aliceKp.did,
        signingPublicKey: aliceKp.signingPublicKey,
        signingPrivateKey: aliceKp.signingPrivateKey,
      },
      displayName: "kars-live-encryption-alice",
      wsFactory,
    });
    bob = await createMeshTransport({
      relayUrl: RELAY,
      registryUrl: REGISTRY,
      identity: {
        amid: bobKp.did,
        did: bobKp.did,
        signingPublicKey: bobKp.signingPublicKey,
        signingPrivateKey: bobKp.signingPrivateKey,
      },
      displayName: "kars-live-encryption-bob",
      wsFactory,
    });

    await alice.connect();
    await bob.connect();
  }, 30_000);

  afterAll(async () => {
    await Promise.all([alice?.disconnect(), bob?.disconnect()]);
    for (const { did, signTimestamp } of [aliceKp, bobKp]) {
      const timestamp = new Date().toISOString();
      const response = await fetch(`${REGISTRY}/v1/agents/${encodeURIComponent(did)}`, {
        method: "DELETE", signal: AbortSignal.timeout(5000),
        headers: { authorization: `Ed25519-Timestamp ${did} ${timestamp} ${signTimestamp(timestamp)}` },
      });
      expect([200, 204, 404]).toContain(response.status);
    }
  }, 15_000);

  it("delivers encrypted A→B, B→A, and A→B messages across ratchet turns", async () => {
    expect(alice.isConnected).toBe(true);
    expect(bob.isConnected).toBe(true);
    expect(alice.getPlaintextPeers()).toEqual([]);
    expect(bob.getPlaintextPeers()).toEqual([]);

    const exchanges = [
      { sender: alice, receiver: bob, from: aliceKp.did, to: bobKp.did },
      { sender: bob, receiver: alice, from: bobKp.did, to: aliceKp.did },
      { sender: alice, receiver: bob, from: aliceKp.did, to: bobKp.did },
    ];
    for (const [turn, { sender, receiver, from, to }] of exchanges.entries()) {
      const payload = { id: crypto.randomUUID(), text: "Private roundtrip — café ✓", turn };
      await sender.send(to, payload);
      const received = await receiver.waitForMessage((content, peer, security) =>
        peer === from ? { content, security } : null, 10_000);
      expect(received).toEqual({ content: payload, security: "encrypted" });
      const wire = frames.at(-1)!;
      expect(wire.from).toBe(from);
      expect(wire.to).toBe(to);
      expect(wire.plaintext).not.toBe(true);
      expect(wire.header).toEqual(expect.objectContaining({
        dh: expect.any(String), pn: expect.any(Number), n: expect.any(Number),
      }));
      expect(typeof wire.ciphertext).toBe("string");
      expect(Buffer.from(wire.ciphertext as string, "base64").equals(Buffer.from(JSON.stringify(payload))))
        .toBe(false);
    }
    expect(frames).toHaveLength(3);
  }, 45_000);
});
