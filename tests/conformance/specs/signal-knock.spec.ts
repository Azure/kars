// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Signal KNOCK sequence invariants — Phase 0 scaffold.
 *
 * See internal Phase 1 plan §5.4. KNOCK is AzureClaw's
 * policy-gated session-establishment protocol layered over Signal
 * X3DH; it's the first place where vendored-AgentMesh and AGT-AgentMesh
 * must agree on wire format.
 *
 * Vendor patches this corpus must exercise:
 *   - patch #5: responder passes signedPrekey to initializeResponder
 *   - patch #7: SDK transport wires receive() to the client
 *   - patch #8: initial session message includes KNOCK + X3DH params
 *
 * Anti-patterns caught by the negative cases here (class of bug from
 * production history):
 *   - KNOCK "sent" but never hit the relay socket (no-op 200)
 *   - base64 wrapper masquerading as Signal (no actual encryption)
 *   - initial message had payload but no X3DH params (echo-loop)
 */
import { describe, it } from "vitest";

describe("KNOCK happy-path", () => {
  it.todo("initiator sends KNOCK frame via relay WebSocket");
  it.todo("relay routes KNOCK to target addressed by amid");
  it.todo("target emits KnockReceived event observable by plugin");
  it.todo("target replies with KnockAccepted + its ratchet keypair");
  it.todo("initiator completes Double Ratchet setup on KnockAccepted");
  it.todo("first encrypted application message decrypts on target");
});

describe("KNOCK with trust threshold", () => {
  it.todo("target rejects KNOCK when peer trust score < AGT_TRUST_THRESHOLD");
  it.todo("anonymous peer (score 0) is accepted when threshold == 0");
  it.todo("verified peer (tier 1, score >= 600) is always accepted at threshold 500");
});

describe("KNOCK under relay disruption", () => {
  it.todo("initiator retries with backoff + jitter on relay disconnect during KNOCK");
  it.todo("target survives relay restart and receives a later resend");
  it.todo("in-flight session keys are NOT discarded on relay reconnect");
});

describe("KNOCK wire-shape invariants (vendored ↔ AGT parity)", () => {
  it.todo("KNOCK frame JSON has {type, from, to, x3dh: {idKey, ephKey, prekeyId, signedPrekey}}");
  it.todo("timestamps serialize as RFC3339 with 'Z' suffix (vendor patch #1,#2)");
  it.todo("VendoredAgentMeshProvider and AgtMeshProvider emit byte-identical KNOCK for the same input");
});
