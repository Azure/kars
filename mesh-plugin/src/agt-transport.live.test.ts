// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Phase 6 — Live AGT mesh round-trip smoke test.
 *
 * Exercises the real upstream Microsoft AGT SDK (@microsoft/agent-governance-sdk)
 * against an upstream Python relay+registry running in Docker.
 *
 * This test is gated on the AZURECLAW_LIVE_AGT env var so it only runs when
 * the operator explicitly stands up the relay+registry. CI does NOT run it.
 *
 * Recipe to run locally:
 *   docker build --build-arg COMPONENT=registry -t agentmesh-registry-agt:dev \
 *     -f /path/to/agt/.../docker/Dockerfile /path/to/agt
 *   docker build --build-arg COMPONENT=relay -t agentmesh-relay-agt:dev \
 *     -f /path/to/agt/.../docker/Dockerfile /path/to/agt
 *   docker network create agt-smoke
 *   docker run -d --name agt-smoke-registry --network agt-smoke -p 18082:8082 \
 *     agentmesh-registry-agt:dev
 *   docker run -d --name agt-smoke-relay --network agt-smoke -p 18083:8083 \
 *     -e REGISTRY_URL=http://agt-smoke-registry:8082 agentmesh-relay-agt:dev
 *   AZURECLAW_LIVE_AGT=1 npx vitest run agt-transport.live
 */

import { describe, it, expect, beforeAll, afterAll } from "vitest";
import * as crypto from "node:crypto";
import { createMeshTransport } from "./transport-factory.js";
import type { IMeshTransport } from "./transport-interface.js";

const LIVE = !!process.env.AZURECLAW_LIVE_AGT;
const RELAY = process.env.AGENTMESH_LIVE_RELAY_URL || "http://localhost:18083";
const REGISTRY = process.env.AGENTMESH_LIVE_REGISTRY_URL || "http://localhost:18082";

function newKeypair(): { signingPublicKey: Uint8Array; signingPrivateKey: Uint8Array; did: string } {
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
  const fp = Buffer.from(pub).subarray(0, 8).toString("hex");
  return {
    signingPublicKey: pub,
    signingPrivateKey: priv,
    did: `did:agt:${fp}`,
  };
}

describe.skipIf(!LIVE)("AGT live mesh round-trip", () => {
  let alice: IMeshTransport;
  let bob: IMeshTransport;
  const aliceKp = newKeypair();
  const bobKp = newKeypair();

  beforeAll(async () => {
    process.env.AZURECLAW_MESH_PROVIDER = "agt";
    alice = await createMeshTransport({
      relayUrl: RELAY,
      registryUrl: REGISTRY,
      identity: {
        amid: aliceKp.did,
        did: aliceKp.did,
        signingPublicKey: aliceKp.signingPublicKey,
        signingPrivateKey: aliceKp.signingPrivateKey,
      },
      displayName: "alice",
      plaintextPeers: [bobKp.did],
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
      displayName: "bob",
      plaintextPeers: [aliceKp.did],
    });

    await alice.connect({ displayName: "alice" });
    await bob.connect({ displayName: "bob" });
  }, 30_000);

  afterAll(async () => {
    await alice?.disconnect().catch(() => {});
    await bob?.disconnect().catch(() => {});
  });

  it("delivers an A→B message and observes it in B's inbox", async () => {
    expect(alice.isConnected).toBe(true);
    expect(bob.isConnected).toBe(true);

    const received: Array<{ from: string; payload: unknown }> = [];
    bob.onMessage((from, payload) => {
      received.push({ from, payload });
    });

    await alice.send(bobKp.did, { hello: "from alice", t: Date.now() });

    const msg = await bob.waitForMessage<{ from: string; payload: unknown }>(
      (content, from) =>
        from === aliceKp.did ? { from, payload: content } : null,
      10_000,
    );
    expect(msg).toBeTruthy();
    expect(msg.from).toBe(aliceKp.did);
    expect(received.length).toBeGreaterThanOrEqual(1);
  }, 20_000);

  it("delivers a B→A reply", async () => {
    await bob.send(aliceKp.did, { reply: "hi alice" });
    const msg = await alice.waitForMessage<{ from: string; payload: unknown }>(
      (content, from) =>
        from === bobKp.did ? { from, payload: content } : null,
      10_000,
    );
    expect(msg).toBeTruthy();
    expect(msg.from).toBe(bobKp.did);
  }, 20_000);
});
