# Security Audit - JWT RSA verification compatibility

Date: 2026-11-12
Scope: `inference-router/src/providers/jwt.rs`, provider installation,
workspace/router dependency manifests and lockfile, OAuth compatibility fixtures,
`ci/no-custom-crypto.sh`, and advisory rationale documentation.
Gated paths: `inference-router/src/providers/jwt.rs`,
`inference-router/src/providers/mod.rs`.

## Summary

The AWS-LC-only candidate for Azure/kars#594 removed `crypto-bigint` 0.5.5 but
rejected an existing 1024-bit OAuth issuer fixture that the previous RustCrypto
provider accepted. It also changed the accepted RSA key range. A green build was
therefore insufficient evidence of a compatible remediation.

The router now uses jsonwebtoken's supported `CryptoProvider` extension to
delegate all six RSA verification algorithms to released `rsa` 0.10 APIs. AWS-LC
still supplies the signer factory, non-RSA verifiers, and JWK utilities.
`rsa` 0.10 uses patched `crypto-bigint` 0.7.11. No unpublished upstream branch,
custom cryptographic primitive, private RSA operation, or failure-triggered
fallback is introduced.

## T1: New capability / attack surface? (NO new customer capability)

- No endpoint, CLI option, CRD field, network destination, permission, trust
  anchor, or mesh protocol changes.
- RSA selection is by the validated algorithm, never by retrying a failed
  signature under a different backend or algorithm.
- RSA decoding keys must belong to the RSA family. Non-RSA signatures are
  verified by AWS-LC. For legacy oversized Ed25519 public-key encodings, the
  adapter passes only the first 32 bytes to AWS-LC, matching the prior verifier;
  it does not try another key or verification backend after a failure.
- JWK components and PKCS#1 DER both use the same checked public-key constructor.
  DER components are parsed by the maintained `pkcs1` library, not handwritten
  ASN.1 code.

## T2: Security-control change? (Patched arithmetic and weak-key rejection)

- `PublicKey::new_with_size_limits(n, e, 0..=4096)` explicitly preserves the former
  verifier's size bounds while retaining the library's other modulus/exponent
  checks. The new library's default minimum must not silently reject legacy
  issuers, including the 1024-bit regression fixture.
- Existing acceptance of weak RSA keys is not an endorsement of those keys.
  New issuers should use at least 2048 bits. Raising the minimum would be a
  separate customer-facing policy change, not hidden inside a dependency fix.
- RS256/384/512 use the library's hash-bound PKCS#1 v1.5 verification APIs.
  PS256/384/512 use its PSS APIs requiring the JOSE digest-length salt, not
  automatic salt detection.
- The adapter handles public keys only. Variable-time decoding of public
  modulus/exponent bytes does not process a private key.
- Encoded public components are stripped of leading zero bytes before conversion
  and rejected before allocation if their significant modulus or exponent exceeds
  the former checked bounds. This matches the former `BigUint` decoding and
  prevents attacker-controlled zero padding from inflating RSA 0.10's arithmetic
  precision. With 1,024 zero bytes prepended to the same small test modulus, the
  uncorrected adapter allocated 8,256 bits rather than 64.
- The prior Ed25519 verifier consumed the first 32 bytes even when a trusted
  JWK's `x` contained trailing bytes. Although those oversized encodings are
  non-conforming, rejecting them would change existing issuer behavior. The
  adapter preserves that interpretation without accepting a different
  public key or performing private-key operations. Correctly formatted new
  Ed25519 JWKs contain exactly 32 bytes; short inputs now fail closed rather
  than retaining the former verifier's panic.
- Both the former Ed25519 verifier and AWS-LC accepted small-order public keys,
  including the identity key that admits a fixed signature for arbitrary JWT
  claims. Before verification, the adapter rejects weak keys using the existing
  `ed25519-dalek::VerifyingKey::is_weak` API. This intentionally rejects unsafe
  issuer keys, including noncanonical weak encodings accepted by the former
  provider; issuers using them must rotate their keys. Ordinary valid issuer
  keys and their legacy trailing-byte handling remain covered by differential
  tests.
- Issuer, audience, expiry, kid, key-algorithm, scope, and HMAC-confusion checks
  remain in the existing OAuth verification path.
- The crypto-wrapper allowlist adds only this provider file, consistent with the
  existing wrapper policy. The mandatory independent security-review gate is
  unchanged. No advisory exception or audit suppression is added.
- The separate Marvin advisory exception remains documented for the existing
  Sigstore/OpenID Connect RSA dependency; successful audit commands do not mean
  there are no ignored advisories.

## T3: Availability / fail-open risk? (Compatibility tested; unsafe keys rejected)

- All RSA verification uses one selected maintained backend. Errors propagate
  through signature verification and the existing OAuth rejection path.
- There is no unsigned, alternate-padding, alternate-algorithm, or
  alternate-provider retry on a failed signature.
- Provider installation retains the existing process-global initialization
  behavior. Concurrent OAuth and route tests exercise the same installer.
- The synthetic public-only fixture matrix covers 512, 768, 1024, 2048, and
  4096-bit keys and public exponents 3 and 65537. It includes all six RSA
  algorithms where the baseline signing padding fits the key.

## Verification

- Baseline differential run: `cargo test --offline --package
  kars-inference-router --test oauth_crypto_compat` passed five tests with the
  previous RustCrypto provider. Its 28 positive RSA vectors were preserved
  byte-for-byte for the candidate run. A separate session-only RSA 0.9
  primitive probe also verified signatures with 1,024 leading zero bytes in
  both public components and checked the old exponent bounds.
- Candidate: `cargo test --locked --offline --package kars-inference-router
  --test oauth_crypto_compat` passed ten tests. Coverage includes the actual Kars
  `verify_access_token` API, PKCS#1 DER/PEM and SPKI PEM, tampered claims and
  signatures, malformed lengths/DER, wrong key family/audience, padded public
  components including 1,024 leading zero bytes, invalid/empty components,
  over-limit moduli/exponents, non-JOSE PSS salt lengths, ES256/ES384 high- and
  low-S signatures, and Ed25519 keys with 0, 1, or 1,024 trailing bytes;
  incorrect, short, and small-order Ed25519 keys are rejected. The weak-key test
  reproduces a message-independent forged signature through the actual OAuth
  path before the fix. The ECDSA public-only fixtures were generated with the
  former provider's RustCrypto curve libraries and verified by that provider
  before candidate verification.
  Wrong-salt fixtures are real signatures generated and rejected by the former
  RSA implementation, not random bytes.
- A session-only provider differential probe compared 704 generated
  ES256/ES384/Ed25519 cases, including valid signatures, high/low-S ECDSA,
  compressed EC keys, and tampering. All 640 standard-key cases matched; the
  64 oversized Ed25519 cases exposed the incompatibility fixed above. All 704
  normal-key cases match with the final provider. A separate weak-key probe
  demonstrated the intentional security rejection described above.
- `cargo test --locked --offline --package kars-inference-router`: passed.
- `cargo clippy --locked --offline --package kars-inference-router --all-targets
  -- -D warnings`: passed.
- `cargo fmt --all -- --check`: passed.
- `cargo tree --locked --offline -i crypto-bigint`: only 0.7.11 is present.
- `cargo audit --deny warnings --no-fetch`: passed against the cached advisory
  database, retaining the documented existing exceptions.
- `cargo deny --offline check`: advisories, bans, licenses, and sources passed;
  existing non-blocking dependency warnings remain.
- Rust 1.88: an isolated session-local crate compiled this exact provider source
  against jsonwebtoken 10.4.0, RSA 0.10.0, and the existing ed25519-dalek 2.2.0
  dependency. It verified 28 RSA, four ECDSA, and three Ed25519 vectors and
  rejected a weak-key forgery using `cargo +1.88.0 run --locked --offline`.
  This is not a full-workspace MSRV claim; the previously reproduced
  full-workspace 1.88 baseline failure remains separate.
- New-head remote CI, controller signed-OCI evidence, and combined PR review
  remain required. Earlier green checks do not cover this follow-up.

## Verdict

Draft for independent review, not authorization to merge or deploy. Author
review and local evidence support the compatibility approach; a genuine
independent reviewer sign-off is still outstanding. This record supplies only
the author's sign-off, so the two-distinct-reviewer gate is not yet satisfied.
No prior approval for another PR is reused.

Signed-off-by: Copilot App <223556219+Copilot@users.noreply.github.com>
