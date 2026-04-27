/**
 * Signal / X3DH / Double-Ratchet invariants — Phase 0 scaffold.
 *
 * See internal Phase 1 plan §5.4.
 *
 * These invariants MUST hold across every MeshProvider implementation
 * (VendoredAgentMeshProvider, AgtMeshProvider, NullMeshProvider-dev).
 * The Phase 0 landings declare the invariants as `it.todo`; each one
 * is filled in by the PR that lands the corresponding wire support.
 *
 * Source of truth for fixtures (to be vendored in Phase 1, not git
 * submoduled):
 *   - libsignal-protocol test vectors
 *     https://github.com/signalapp/libsignal/tree/main/rust/protocol/tests
 *   - vendor/agentmesh-sdk/README.md (documents 8 patched bugs; each
 *     patch implies a negative test listed below).
 */
import { describe, it } from "vitest";

describe("Signal X3DH — key-exchange shape", () => {
  it.todo("prekey bundle has a non-empty Ed25519 signature (vendor patch #3)");
  it.todo("prekey bundle verifies under the claimed identity key");
  it.todo("x3dh computes identical shared secret on both sides");
  it.todo("x3dh rejects a bundle whose signedPrekey signature is tampered");
  it.todo("x3dh rejects a bundle whose identity key doesn't match registry record");
});

describe("Signal Double Ratchet — symmetric invariants", () => {
  it.todo("initial root key is identical on both sides after X3DH");
  it.todo("chain-key ratchets by one step per message in the same chain");
  it.todo("DH ratchet advances root key when the other side replies");
  it.todo("out-of-order receive within a chain decrypts correctly");
  it.todo("out-of-order receive across chains (skipped DH ratchet) decrypts correctly");
  it.todo("message key is zeroed from memory after decryption");
});

describe("Signal base64 — input hygiene", () => {
  it.todo("base64Decode strips an 'x25519:' key-id prefix before decoding (vendor patch #4)");
  it.todo("base64Decode strips an 'ed25519:' key-id prefix before decoding (vendor patch #4)");
  it.todo("base64Decode rejects invalid-character input with a typed error");
});
