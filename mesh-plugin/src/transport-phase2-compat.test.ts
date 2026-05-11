// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Phase 2 surface compatibility — both transports (vendored A and AGT B)
 * MUST expose the same set of methods so the runtime can swap providers
 * via AZURECLAW_MESH_PROVIDER without code changes.
 *
 * This test pins the IMeshTransport contract for the 6 Phase 2 additions:
 *   lookup, submitReputation, enableKnockEnforcement,
 *   onError, onE2EVerified, onDisconnect.
 *
 * If either implementation drops one of these methods, this test fails.
 * If either renames a method, this test fails. The runtime depends on
 * exactly this surface (see runtimes/openclaw/src/index.ts L555–L2200).
 */

import { describe, it, expect } from "vitest";
import { MeshConnection } from "./connection.js";
import { AgtTransport } from "./agt-transport.js";
import type { IMeshTransport } from "./transport-interface.js";

const PHASE_2_METHODS: Array<keyof IMeshTransport> = [
  "lookup",
  "submitReputation",
  "enableKnockEnforcement",
  "onError",
  "onE2EVerified",
  "onDisconnect",
];

const identity = {
  amid: "did:agentmesh:compat-test",
  did: "did:agentmesh:compat-test",
  agentId: "did:agentmesh:compat-test",
  signingPrivateKey: new Uint8Array(32),
  signingPublicKey: new Uint8Array(32),
  // MeshConnection consumes this opaquely; AgtTransport only needs the typed fields.
  sdkIdentity: {} as unknown,
};

describe("IMeshTransport Phase 2 surface — both adapters expose required methods", () => {
  const vendored = new MeshConnection({
    relayUrl: "ws://localhost:7777",
    registryUrl: "http://localhost:7778",
    identity: identity as unknown as Parameters<typeof MeshConnection>[0]["identity"],
  });

  const agt = new AgtTransport({
    relayUrl: "ws://localhost:7777",
    registryUrl: "http://localhost:7778",
    identity,
  });

  for (const method of PHASE_2_METHODS) {
    it(`vendored MeshConnection exposes ${String(method)}`, () => {
      expect(typeof (vendored as unknown as Record<string, unknown>)[method as string]).toBe(
        "function",
      );
    });

    it(`AGT AgtTransport exposes ${String(method)}`, () => {
      expect(typeof (agt as unknown as Record<string, unknown>)[method as string]).toBe(
        "function",
      );
    });
  }

  it("enableKnockEnforcement is a no-op on AGT (always-on); does not throw", () => {
    expect(() => agt.enableKnockEnforcement()).not.toThrow();
  });

  it("onError/onE2EVerified/onDisconnect accept handlers without throwing pre-connect", () => {
    expect(() => agt.onError(() => {})).not.toThrow();
    expect(() => agt.onE2EVerified(() => {})).not.toThrow();
    expect(() => agt.onDisconnect(() => {})).not.toThrow();
    expect(() => vendored.onError(() => {})).not.toThrow();
    expect(() => vendored.onE2EVerified(() => {})).not.toThrow();
    expect(() => vendored.onDisconnect(() => {})).not.toThrow();
  });

  it("lookup returns null on unreachable registry (both adapters)", async () => {
    expect(await agt.lookup("did:agentmesh:nobody")).toBeNull();
    expect(await vendored.lookup("did:agentmesh:nobody")).toBeNull();
  });

  it("submitReputation returns false on unreachable registry (both adapters)", async () => {
    expect(await agt.submitReputation("did:agentmesh:nobody", "sess-1", 0.8, ["test"]))
      .toBe(false);
    expect(await vendored.submitReputation("did:agentmesh:nobody", "sess-1", 0.8, ["test"]))
      .toBe(false);
  });
});
