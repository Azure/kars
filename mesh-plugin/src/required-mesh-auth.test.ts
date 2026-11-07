// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { RequiredMeshAuth } from "./required-mesh-auth.js";

const did = `did:mesh:${"a".repeat(32)}`;
describe("shared authentication facade ingress custody", () => {
  it("forwards invocation capture only while authenticated and preserves callback custody", async () => {
    let callback!: (...args: unknown[]) => unknown;
    let capture!: () => unknown;
    const raw = {
      isConnected: false, currentDid: did,
      connect: async () => { raw.isConnected = true; }, disconnect: async () => { raw.isConnected = false; },
      onDisconnect: vi.fn(),
      onMessage: (handler: typeof callback, captureIngress?: () => unknown) => { callback = handler; capture = captureIngress!; },
    };
    const auth = new RequiredMeshAuth(raw, { publish: vi.fn(), revoke: vi.fn(), fatal: vi.fn(),
      request: async method => method === "GET" ? { access_token: "test-token", token_type: "Bearer", expires_in: 3000 }
        : { did, tier: "verified", verified_app_id: "test-app", verified_tenant_id: "test-tenant" } });
    let owner = "old";
    const handler = vi.fn(); const captureOwner = vi.fn(() => owner);
    auth.client.onMessage(handler, captureOwner);
    expect(() => capture()).toThrow("unavailable");
    expect(captureOwner).not.toHaveBeenCalled();
    await auth.connect();
    try {
      const captured = capture(); owner = "new";
      callback(did, { content: "answer" }, "encrypted", captured);
      expect(handler).toHaveBeenCalledWith(did, { content: "answer" }, "encrypted", "old");
    } finally { await auth.close(); }
    expect(() => capture()).toThrow("unavailable");
    callback(did, {}, "encrypted", "new");
    expect(handler).toHaveBeenCalledOnce();
  });
});
