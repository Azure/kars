# Security Audit - JWT and signed-OCI verification compatibility

Date: 2026-11-12
Scope: `inference-router/src/providers/jwt.rs`, provider installation,
`inference-router/src/mcp/oauth.rs`, workspace/router dependency manifests and
lockfile, OAuth compatibility fixtures, `controller/src/policy_fetcher.rs`,
controller Sigstore/OCI integration fixtures, `ci/no-custom-crypto.sh`, and
advisory rationale documentation.
Gated paths: `inference-router/src/providers/jwt.rs`,
`inference-router/src/providers/mod.rs`, `inference-router/src/mcp/oauth.rs`,
`controller/src/policy_fetcher.rs`.

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

This record also documents the production Sigstore/OCI verifier changes for
Azure/kars#594. Those controller changes are a separate security-sensitive
surface; the earlier JWT-only review does not cover them. Their tests and
limitations are recorded in T4 and still require an independent review.

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
  `ed25519-dalek::VerifyingKey::is_weak` API. The OAuth path applies the same
  check even when an embedding application already installed another
  jsonwebtoken provider. This intentionally rejects unsafe issuer keys,
  including noncanonical weak encodings accepted by the former provider;
  issuers using them must rotate their keys. Ordinary valid issuer keys and
  their legacy trailing-byte handling remain covered by differential tests.
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
  A host that installs another provider first keeps that provider, so legacy
  RSA and oversized Ed25519 key compatibility depend on that provider; the
  OAuth weak-Ed25519-key rejection is enforced independently of it.
- The synthetic public-only fixture matrix covers 512, 768, 1024, 2048, and
  4096-bit keys and public exponents 3 and 65537. It includes all six RSA
  algorithms where the baseline signing padding fits the key.

## T4: Sigstore signed-OCI verification

### Identity and candidate semantics

- Production verification uses Sigstore 0.15 trusted signature-layer
  discovery with Kars's configured trust root. Sigstore validates the
  certificate trust and Rekor evidence, including SET/proof/body-certificate
  binding for v0.3 referrers. It does not verify the legacy artifact signature
  or referrer DSSE PAE signature before returning those layers. Kars applies
  its configured issuer/SAN policy and explicitly verifies the actual
  signature over each layer's signed bytes with the validated certificate key
  for both legacy and referrer candidates.
- Sigstore's constraint contract is any-valid-signature: a candidate that
  fails a constraint is skipped if another candidate satisfies it. Kars now
  preserves this contract for legacy Rekor evidence: binding errors remove
  only that legacy candidate before the existing identity/signature
  constraints run. A malformed or unrelated legacy layer cannot poison a
  valid authorized legacy or v0.3 referrer layer, and a rejected candidate is
  never allowed to authorize. If every candidate fails, verification remains
  fail closed. Candidates are checked one at a time through borrowed
  one-element slices, so Kars does not clone registry-controlled layer,
  payload, or Rekor-bundle bytes.
- The legacy `.sig` validator accepts Rekor `hashedrekord` and `rekord`
  `0.0.1` bodies. It recomputes the advertised digest from the exact signed
  layer bytes for SHA-256, SHA-384, or SHA-512 on `hashedrekord`; `rekord`
  `0.0.1` is constrained to its schema's SHA-256 hash. It requires the logged
  signature bytes to equal the OCI signature. `rekord` requires
  `signature.format` to be `x509`; the `hashedrekord` schema has no `format`
  field, and Kars accepts its canonical absence. Advertised hexadecimal
  digests are decoded before comparison, so uppercase and lowercase hex
  spellings bind to the same exact digest bytes. For `rekord`, raw
  `data.content` is not relied on: Rekor's
  canonical entry omits it, and the bound digest is recomputed from the signed
  OCI layer. These formats follow the [Cosign legacy bundle example](https://github.com/sigstore/cosign/blob/main/specs/SIGNATURE_SPEC.md#properties)
  and [Rekor hashedrekord schema](https://github.com/sigstore/rekor/blob/main/pkg/types/hashedrekord/v0.0.1/entry.go)
  and [rekord canonicalization](https://github.com/sigstore/rekor/blob/main/pkg/types/rekord/v0.0.1/entry.go).
- Sigstore exposes the accepted legacy certificate's parsed verification key,
  issuer, and SAN identity but does not retain its original DER bytes.
  Previously Kars fetched the mutable `.sig` tag a second time to compare the
  Rekor certificate to a second descriptor snapshot. That extra fetch has
  been removed. The Rekor-logged certificate is parsed directly and must have
  the same signer identity and Fulcio issuer as the certificate Sigstore
  validated from its snapshot; its parsed SPKI must match exactly and that key
  must verify the accepted OCI layer signature. This binds
  authorization-relevant identity and key
  across a mutable tag change without trusting a second descriptor read.
  Byte-for-byte certificate DER equality is not asserted because the upstream
  API discards the original DER; a different certificate with the same key,
  issuer, and SAN is treated as the same signer identity for this check.
- OCI v0.3 referrer bundles continue through Sigstore's protobuf bundle
  verification path for certificate trust, Rekor SET, inclusion
  proof/checkpoint, and subject-digest validation. Kars applies the same
  configured issuer/SAN and actual DSSE PAE signature checks. A failed legacy
  candidate does not preempt a valid referrer candidate.

### Failure handling, network effects, and limitations

- Malformed Rekor base64/JSON/hexadecimal digest, unsupported kind/version/hash
  algorithm or `rekord` signature format, incorrect payload digest/signature,
  malformed certificate, missing or mismatched issuer/SAN, unsupported key,
  and a certificate key that does not verify the layer signature all disqualify
  that candidate.
  These conditions do not suppress a different valid candidate. If no
  candidate remains or no candidate matches signer policy, policy retrieval
  fails closed.
- Kars no longer performs a second authenticated manifest read of the legacy
  `<digest>.sig` tag. Sigstore owns legacy-tag and referrer discovery using the
  configured registry authentication; Kars only performs the existing
  digest-pinned policy pull after verification succeeds. There is no new
  registry destination, credential, trust anchor, or protocol.
- Local tests use an OCI Distribution API fixture and test-only signing keys,
  certificates, and Rekor material. They exercise actual signature/SET
  verification but do not substitute for a live registry, Fulcio, or Rekor
  transaction. A previous-head session-only live-registry probe against public
  Kubewarden GHCR legacy and referrer signatures passed through the real
  Sigstore public trust root; that probe was not a full Kars policy pull, and it
  must be rerun against the final head before being counted as current-head
  evidence.
- The local fixture's changing `.sig` manifest snapshots model the mutable-tag
  race: Sigstore's two manifest reads receive the original snapshot; a
  hypothetical third read receives a changed descriptor whose certificate
  bytes match the SET-signed Rekor body. The test asserts that only Sigstore's
  two reads occur and rejects the candidate because the Rekor cert's
  key/identity do not match the accepted signer. Production no longer
  refetches the tag.
- RSA-PSS is not selected by the supported Cosign legacy certificate path.
  Cosign's `ValidateAndUnpackCertWithIntermediates` calls
  `signature.LoadVerifier(cert.PublicKey, crypto.SHA256)`; Sigstore Go's
  generic `LoadVerifier` dispatches RSA keys to PKCS#1 v1.5, while RSA-PSS
  requires an explicit `LoadRSAPSSVerifier` call. Likewise, Sigstore 0.15's
  automatic `CosignVerificationKey::try_from` maps an `rsaEncryption` SPKI to
  `RSA_PKCS1_SHA256`; its explicit scheme constructor can select PSS, but a
  legacy Cosign signature object carries no algorithm marker to select it.
  Kars retains the default legacy Cosign behavior rather than guessing an
  alternate scheme. This documents the upstream default verifier behavior,
  not a claim that the Cosign wire specification forbids PSS or that the
  general Sigstore algorithm registry lacks RSA-PSS support.
  References: [Cosign verifier](https://github.com/sigstore/cosign/blob/main/pkg/cosign/verify.go),
  [Sigstore Go verifier dispatch](https://github.com/sigstore/sigstore/blob/main/pkg/signature/verifier.go),
  [Cosign signature specification](https://github.com/sigstore/cosign/blob/main/specs/SIGNATURE_SPEC.md),
  and [Sigstore Rust 0.15 key conversion](https://docs.rs/sigstore/0.15.0/src/sigstore/crypto/verification_key.rs.html).

### Signed-OCI evidence

- The legacy OCI integration fixture signs a canonical SimpleSigning payload
  with a test certificate and signs the canonical Rekor SET with a test Rekor
  key. Positive cases include `hashedrekord` SHA-256/P-256,
  SHA-384/P-384, and SHA-512/P-521, plus Rekor `rekord` SHA-256/P-256. The
  SHA-384 and SHA-512 cases use certificates and signatures whose artifact
  signature hash matches the Rekor `data.hash` algorithm. A SET-signed
  `hashedrekord` fixture omits the
  schema-inapplicable `signature.format`, and another uses uppercase digest
  hex. Negative cases include wrong issuer/SAN, tampered signature/artifact,
  signed noncanonical policy bytes, validly SET-signed mismatched payload
  hash/signature/certificate, non-x509 `rekord`, and the two-snapshot
  certificate substitution.
- Mixed-signature OCI fixtures cover one valid authorized legacy signature
  alongside a second legacy candidate with a SET-signed mismatched hash or a
  malformed certificate annotation. Another fixture combines a valid signed
  OCI-referrer bundle with a legacy candidate whose changed third `.sig` tag
  snapshot matches the SET certificate but not Sigstore's accepted signer;
  verification succeeds on the referrer and asserts no third tag read occurs.
  These pass because the invalid candidate is skipped while the valid signer
  is still independently checked.
- OCI-referrer integration positives and negatives use a real Sigstore v0.3
  protobuf bundle, DSSE signature, certificate, SET, Merkle proof, and signed
  checkpoint. Tests reject wrong issuer/SAN, wrong subject, invalid DSSE
  signature, and tampered artifact bytes.

## Verification

- `cargo test --locked --offline --package kars-controller
  policy_fetcher::tests::signed_oci -- --nocapture`: passed all three
  signed-OCI integration tests after adding multi-signature availability,
  changed-tag/referrer, canonical `rekord`, SHA-384/SHA-512, schema-correct
  `hashedrekord`, uppercase digest, and borrowed candidate evaluation
  coverage.
- `cargo test --locked --offline --package kars-controller`: passed 863 unit
  tests plus the phase-taxonomy guard (1 test).
- `cargo clippy --locked --offline --package kars-controller --all-targets --
  -D warnings`, `cargo fmt --all -- --check`, `git diff --check`, and
  `bash ci/no-custom-crypto.sh`: passed on the changed worktree.
- New-head remote CI has not yet been run for this follow-up. Previous-head
  green CI does not validate these changes; the PR must remain draft until
  current-head checks and independent review are complete.
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
- `cargo test --locked --offline --package kars-inference-router --test
  oauth_preinstalled_provider` passed: OAuth rejects a weak-key forgery and
  accepts a valid Ed25519 token when AWS-LC was installed before Kars.
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
## Verdict

Draft for independent review, not authorization to merge or deploy. Author
review and local evidence support the compatibility approach; a genuine
independent reviewer sign-off is still outstanding. This record supplies only
the author's sign-off, so the two-distinct-reviewer gate is not yet satisfied.
No prior approval for another PR is reused.

Signed-off-by: Copilot App <223556219+Copilot@users.noreply.github.com>
