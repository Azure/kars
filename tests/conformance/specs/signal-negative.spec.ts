/**
 * Signal tamper / replay / denial-of-service invariants — Phase 0 scaffold.
 *
 * See docs/implementation-plan.md §5.4 + §0.2 principle #8.
 *
 * Every negative case here maps to a production-history bug or a
 * documented Signal-protocol attack surface:
 *   - ciphertext-bit-flip (tamper)
 *   - replay with prior ratchet key
 *   - prekey exhaustion DoS
 *   - malformed frame flood
 *   - session-clobber race (two KNOCKs, vendor patch #10)
 */
import { describe, it } from "vitest";

describe("Ciphertext integrity", () => {
  it.todo("bit-flipped ciphertext fails Poly1305 MAC → DecryptError");
  it.todo("ciphertext with swapped XSalsa20 nonce → DecryptError");
  it.todo("truncated ciphertext (length mutation) → DecryptError");
  it.todo("DecryptError does NOT leak partial plaintext via error message");
});

describe("Replay resistance", () => {
  it.todo("identical encrypted frame replayed after successful decrypt → rejected");
  it.todo("replay attempt does NOT advance the victim's chain key");
  it.todo("replay attempt IS recorded as an AGT audit event (receipt id)");
});

describe("Session clobber / initiate-while-active", () => {
  it.todo("SessionManager.initiateSession returns {reused:true} when crypto already has a session from an incoming KNOCK (vendor patch #10)");
  it.todo("double-initiate does NOT reset ratchet state");
  it.todo("double-initiate emits a warning log line (not silent)");
});

describe("Denial-of-service surfaces", () => {
  it.todo("prekey bundle served only when requester presents valid registry token");
  it.todo("relay connection throttled per-peer-amid (rate-limiter is an AGT concern; we verify the hook fires)");
  it.todo("malformed frame (not valid JSON / missing type field) is dropped with a counter increment, no crash");
});
