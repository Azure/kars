# Security audit — Phase 1 handoff crypto submodule extraction

**Capability:** `inference_router::handoff::crypto` — extraction of the
AES-256-GCM blob cipher, HKDF-SHA256 key derivation, gzip-JSON
serialization, and SHA-256 verification hash from `handoff::mod` into
its own submodule. Phase 1 hotspot decomposition (plan §4.2).

**Branch:** `phase1/handoff-crypto-extract`
**Date:** 2026-04-25

## 1. Summary

Pure refactor: moves the existing handoff crypto code path from
`inference-router/src/handoff/mod.rs` (2075 → 1954 LOC) into a new
`inference-router/src/handoff/crypto.rs` (272 LOC). All public names
(`HANDOFF_STATE_VERSION`, `EncryptedHandoffBlob`, `serialize_state`,
`deserialize_state`, `encrypt_state`, `decrypt_state`,
`compute_verification_hash`) are re-exported from `crate::handoff` so
every existing call-site under `crate::routes::*`, the binary
(`src/main.rs`), and the tests keeps compiling unchanged. Plan §4.2
slates `handoff/mod.rs` for `client/server/crypto` decomposition with a
Phase 1 cap of 1800 LOC; this is the `crypto` slice of that work.

Behavioural change: **none.** The cipher, the HKDF context tag
(`b"azureclaw-handoff-v1"`), the nonce length (96 bits), the random
source, the integrity-hash algorithm, the wire format
(`EncryptedHandoffBlob` field names + base64 encoding) — all preserved
bit-for-bit. The 585 passing lib tests include the existing handoff
suite plus 7 new positive/negative tests in
`handoff::crypto::tests` (round-trip; tampered ciphertext → AEAD reject;
wrong key → AEAD reject; truncated nonce → length reject; substituted
verification_hash → integrity reject; SHA-256 known vector;
`compute_verification_hash` ↔ `hex_sha256` equivalence).

## 2. Threat model delta

None. The handoff crypto path was already the documented Phase H1
mechanism. Moving it to a submodule changes the file boundary, not the
trust boundary. No new caller, no new key path, no new wire format.

## 3. OWASP mapping

- **OWASP LLM Top 10 v2.0 — LLM02 (Sensitive Information Disclosure):**
  the encrypted handoff blob carries the agent's compressed state
  (chat, trust, audit, sub-agent workspaces). Confidentiality continues
  to rest on AES-256-GCM with an HKDF-derived key. The new tests
  explicitly cover the tamper / wrong-key / truncated-nonce reject
  paths so a regression cannot silently fall back to plaintext
  delivery.
- **OWASP MCP Top 10 — A04 (Insecure Session Management):** the
  per-handoff random salt + random 96-bit nonce + GCM auth tag are
  preserved. No nonce reuse, no static IV.

## 4. AuthN / AuthZ path

Unchanged. The handoff endpoints in `routes::handoff` still carry the
two-token (admin + handoff) auth check via
`handoff_auth_middleware` / `handoff_init_auth_middleware` /
`handoff_status_auth_middleware`, none of which moved. The crypto
module is invoked only from inside an already-authenticated request
context.

## 5. Secret + key custody

Unchanged. The AES-256 key is derived per call from the caller-supplied
`shared_secret` via HKDF-SHA256 with a per-call random salt. No key
material persists in the module beyond the stack frame of
`encrypt_state` / `decrypt_state`. The handoff token itself (Phase H1
`shared_secret` source) lives in `handoff::token::HandoffTokenStore`
in CLI/router process memory only and was unchanged by this PR.

Agent (UID 1000) has no access — the router process is UID 1001 and
the handoff endpoints reject prompt-injected localhost calls per the
existing two-token middleware.

## 6. Egress surface delta

Zero. Pure CPU + heap. No network, no DNS, no file I/O.

## 7. Audit events emitted

Unchanged. The `routes::handoff` request handlers continue to call the
existing `AuditSink` events (`handoff.init`, `handoff.snapshot`,
`handoff.transfer`, `handoff.complete`). The crypto module itself
emits no events; it is invoked beneath the audit boundary.

## 8. Failure mode

**Fail-closed by construction.**
- Wrong key → AEAD authentication fails → `Err`.
- Tampered ciphertext → AEAD authentication fails → `Err`.
- Truncated nonce → explicit length check → `Err` before key
  derivation.
- Substituted `verification_hash` matching a different plaintext →
  decrypt succeeds but the post-decrypt integrity hash check rejects.
- Bad gzip stream → `deserialize_state` returns `Err`.

Every failure path returns `Err(String)`; no panic, no fall-through to
a "best-effort" plaintext path. Tests cover every documented failure
mode.

## 9. Negative-test coverage

Seven new tests in `handoff::crypto::tests`:

- `hex_sha256_known_vector` — RFC 6234 known answer for SHA-256.
- `encrypt_decrypt_round_trip` — happy path + version stamp.
- `decrypt_rejects_tampered_ciphertext` — bit-flip → AEAD reject.
- `decrypt_rejects_wrong_key` — different shared secret → AEAD reject.
- `decrypt_rejects_invalid_nonce_length` — truncated nonce → length
  reject before key derivation.
- `decrypt_rejects_tampered_verification_hash` — defence-in-depth
  integrity hash on plaintext (catches the case where a fully-valid
  ciphertext-of-different-plaintext is substituted with the original
  `verification_hash`).
- `compute_verification_hash_matches_hex_sha256` — equivalence.

Plus the pre-existing handoff suite (rest of the 585 lib tests)
covering the integration round-trip, tampered-blob rejection at the
HTTP boundary, wrong-token rejection, etc.

## 10. Vendored / third-party dependency delta

None. Same crates (`aes-gcm`, `hkdf`, `sha2`, `flate2`, `base64`,
`rand`, `serde`, `serde_json`) — moved from one source file to
another, no `Cargo.toml` change.

`ci/no-custom-crypto.sh` allowlist gains
`inference-router/src/handoff/crypto.rs` with comment
"extracted crypto submodule (AES-256-GCM + HKDF-SHA256 + integrity
hash); single allow-listed home for the handoff blob cipher". The
`mod.rs` allowlist row is retained because the file still imports
`base64` (used by tests).

## 11. Sign-offs

Phase 1 hotspot decomposition pass #2 (plan §4.2 / §7 item 8). Pure
refactor, all 585 lib tests green pre+post, clippy clean, all 6 CI
gates green.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
