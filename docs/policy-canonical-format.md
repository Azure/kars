# Canonical Policy Format (S12.a)

This document defines the **byte-stable** serialization rules for AzureClaw policy artifacts that are sealed as signed OCI blobs.

The artifact digest in `OciArtifactRef.digest` covers the canonical bytes specified here. Two functionally equivalent allowlists MUST produce identical digests; any deviation from these rules is a bug.

## Why canonicalization matters

Cosign signs bytes, not semantics. If an artifact's payload differs from its semantic intent (e.g., re-ordered keys, alternative case, default ports omitted vs. explicit), then:

- The signature still verifies (bytes match)
- But operators reviewing the source manifest may believe a different policy is in force
- And drift detection (`AllowlistDrift` condition in S12.e) becomes unreliable

A canonical form eliminates ambiguity so two operators producing the same logical allowlist always produce the same digest.

## Egress Allowlist v1 (`application/vnd.azureclaw.egress-allowlist.v1+yaml`)

### Wire format

```yaml
apiVersion: azureclaw.dev/v1alpha1
kind: EgressAllowlist
metadata:
  generation: <monotonic positive integer, see "Replay protection" below>
spec:
  endpoints:
    - host: api.github.com
      port: 443
    - host: dev.azure.com
      port: 443
```

### Canonicalization rules

1. **YAML serializer:** block style only; no flow style; no anchors/aliases. UTF-8 encoded; LF (`\n`) line endings; trailing newline.
2. **Top-level keys** appear in this exact order: `apiVersion`, `kind`, `metadata`, `spec`.
3. **`apiVersion`** MUST be the literal `azureclaw.dev/v1alpha1`. Reject any other value at consumer side.
4. **`kind`** MUST be the literal `EgressAllowlist`.
5. **`metadata.generation`** MUST be a positive integer, monotonically increasing per logical policy lineage. New seals derived from a prior digest MUST set `generation = previous + 1`. Replay protection: a controller comparing two refs for the same lineage MUST reject a digest whose `generation` is less than the current observed `generation`.
6. **`spec.endpoints`** is a list of `{host, port}` objects. Other fields (`methods`, `paths`) are deliberately excluded from v1 to keep the canonicalization surface small; future versions bump the `+yaml` suffix.
7. **Endpoint hostnames** (`host`):
   - Lowercased ASCII.
   - **IDNA 2008** Punycode-encoded for any non-ASCII characters (use `idna.encode` / `tonic` / `punycode-rs`; never raw Unicode).
   - No trailing dot.
   - No `*` wildcards in v1 (deferred to v2).
   - Reject leading/trailing whitespace, embedded control bytes, and any byte not in `[a-z0-9.-]`.
8. **Endpoint ports** (`port`): integer in `[1, 65535]`. **Always explicit** — no implicit-443 defaults. Reject `0` and out-of-range.
9. **Endpoint deduplication:** the producer MUST collapse `(host, port)` tuples that compare bytewise-equal after rule #7 normalization. The canonical document contains zero duplicates.
10. **Endpoint sort order:** the canonical document sorts endpoints lexicographically, primarily by `host` (byte-by-byte after lowercasing), secondarily by `port` ascending.
11. **YAML key order within each endpoint:** `host` then `port`. Producers MUST emit fields in this order.
12. **No comments** in the canonical document. Comments are stripped before signing.
13. **No empty maps or empty lists** (`metadata: {}` is invalid; omit instead). The one exception is `spec.endpoints: []` for an explicit empty allowlist (which means "deny all egress except the always-allowed defaults") — this MUST be emitted as the four bytes `[]\n` after the `endpoints:` key, not as an indented empty block.

### Signing

```
oras push <registry>/<repository>@sha256:<digest> \
    --artifact-type application/vnd.azureclaw.egress-allowlist.v1+yaml \
    canonical.yaml
cosign sign <registry>/<repository>@sha256:<digest>
```

The `oras push` step computes the `sha256` digest over the raw YAML bytes; the producer MUST verify the digest matches the canonical bytes it just computed before invoking `cosign sign`. Mismatch is a producer bug — abort.

### Verification (consumer)

Two-step:

1. **Cryptographic validity:** cosign signature exists, chains to a Fulcio root, certificate not expired at signing time.
2. **Authority:** signer identity (Fulcio cert SAN + issuer claim) matches an entry in the cluster `SignerPolicy` ConfigMap. The authoritative configuration lives in the cluster-scoped ConfigMap `azureclaw-signer-policy` in the controller namespace (provisioned by the Helm chart via `signerPolicy.fulcioIssuers` / `signerPolicy.sanPatterns` — see `deploy/helm/azureclaw/templates/signer-policy-configmap.yaml`). Env vars (`AZURECLAW_SIGNER_FULCIO_ISSUERS` / `AZURECLAW_SIGNER_SAN_PATTERNS`) remain as an emergency-override fallback when the ConfigMap is **absent**; a *malformed* ConfigMap surfaces as `AllowlistVerified=False / SignerPolicyMalformed` and does **not** silently fall back to the env path.

After verification, parse the YAML and revalidate every canonicalization rule above. Reject (set `AllowlistVerified=False`, reason `CanonicalFormViolation`) on any mismatch — even a valid signature over malformed canonical bytes is not authority.

## Forward compatibility

When v2 is needed (e.g., to reintroduce wildcards, methods, paths, CIDR ranges):

- Bump artifactType to `application/vnd.azureclaw.egress-allowlist.v2+yaml`.
- Define new canonicalization rules. v2 consumers SHOULD also accept v1 artifacts; v1 consumers MUST reject v2 artifacts (forward incompatibility is the safe direction).
- The on-CR `OciArtifactRef.artifactType` is the authority — controllers select the canonical-format codec by media-type, not by `apiVersion` field inside the document.

## Status

| Slice | Owns |
|---|---|
| **S12.a** | Format definition, CRD field, no behavior *(landed)* |
| S12.b | Controller fetcher + format validator (status-only) *(landed)* |
| S12.c | CLI canonical serializer + signing *(landed)* |
| S12.d | `SignerPolicy` ConfigMap + identity-pinned authority *(landed)* |
| S12.e | Authoritative ref mode (controller derives NetworkPolicy egress from verified canonical bytes; fail-closed with LKG fallback) *(landed)* |
| S12.f | Router-side blocked-egress visibility *(landed)* |
| **S12.g** *(this slice — landed)* | Sign-by-default + `--emit-manifest` GitOps mode (S12 close-out) |

## Producer (CLI)

S12.c shipped the producer side of this format inside the CLI as
`azureclaw egress … --sign`. **As of S12.g, signing is default-on**
whenever the producer flow runs — that is, whenever the operator
combines `azureclaw egress` with `--enforce` or `--approve <domain>`.
Pass `--no-sign` to opt out (the CLI prints a loud yellow warning that
the controller will emit `AllowlistVerified=False/SignerPolicyMissing`
and refuse the artifact in authoritative mode). The on-CR
`allowlistRef` is **authoritative** since S12.e — the controller
derives NetworkPolicy egress from the verified canonical bytes. See
[`docs/operations/gitops.md`](operations/gitops.md) for the full
sign-by-default + emit-manifest walkthrough.

What `--sign` does, in order:

1. Reads the live `ClawSandbox` via `kubectl get -o json` to capture
   `metadata.generation` and the just-mutated
   `spec.networkPolicy.allowedEndpoints`.
2. Builds canonical YAML using the deterministic encoder in
   `cli/src/commands/egress/sign.ts`. The encoder is hand-rolled (not
   `js-yaml`/`yaml` defaults) because the rules above are stricter than
   any general-purpose YAML emitter — block style only, fixed key
   order, IDNA-2008 host normalization, lexicographic sort, no
   anchors/aliases/comments, LF-only with trailing newline. The
   producer's local digest is recomputed and compared to the digest
   reported by `oras push`; mismatch aborts before signing.
3. Pushes the canonical bytes as an OCI artifact via
   `oras push <registry>/<repository>:latest --artifact-type
   application/vnd.azureclaw.egress-allowlist.v1+yaml`.
4. Signs the resulting digest via `cosign sign --yes
   <registry>/<repository>@<digest>` in one of three modes:
   - `keyless` (auto-default when stdout is a TTY and no token env is
     set) — interactive Fulcio OIDC.
   - `identity-token` (auto-default when `SIGSTORE_ID_TOKEN` or
     `OIDC_TOKEN` env is set, e.g., GitHub Actions OIDC) — passes the
     token via `--identity-token`.
   - `keyed` (selected by `--sign-mode keyed --sign-key <ref>`) —
     supports a local key file or a KMS URI like `azurekms://kv/key`.
5. Patches `ClawSandbox.spec.networkPolicy.allowlistRef` via
   `kubectl patch --type=merge` with the `{registry, repository,
   digest, artifactType}` tuple.

**Fail-closed:** signature push failure aborts before patching the CR
— no orphan `allowlistRef` pointing at an unsigned artifact ever lands
on a `ClawSandbox`. The `oras` and `cosign` binaries must be present
in `$PATH`; missing-binary errors include the upstream install URL
(see `https://oras.land/docs/installation` and
`https://docs.sigstore.dev/cosign/installation`).

## Authoritative mode (S12.e — current behavior)

When `spec.networkPolicy.allowlistRef` is set, the controller treats
the verified canonical artifact as the **source of truth** for
NetworkPolicy egress. The decision tree per reconcile:

1. **No `allowlistRef`** → legacy inline path: NP egress derived from
   inline `allowedEndpoints`. `AllowlistAuthoritative=False/Inline` is
   surfaced if inline is non-empty (otherwise no S12 condition is
   emitted).
2. **`allowlistRef` set + verify ok** → NP egress derived from the
   artifact. Inline (if any) is ignored. Surfaced:
   - `AllowlistVerified=True/Verified`
   - `AllowlistAuthoritative=True/Verified`
   - `AllowlistDrift=True/InlineDiffersFromArtifact` if inline is
     non-empty and disagrees with the artifact (set-equal check after
     host-lowercase + default-port-443 normalization)
3. **`allowlistRef` set + verify fails + last-known-good (LKG)
   present** → NP egress derived from LKG. Surfaced:
   - `AllowlistVerified=False/<reason>` (e.g. `SignerPolicyMissing`,
     `Unauthorized`, `DigestMismatch`, `IdentityMismatch`,
     `CanonicalFormViolation`)
   - `AllowlistAuthoritative=False/StaleLKG`
4. **`allowlistRef` set + verify fails + no LKG** → fail closed:
   - `AllowlistAuthoritative=False/FailedClosed`
   - the sandbox NetworkPolicy is written with **only** the
     baseline rules (DNS, IMDS UID-restricted, HTTPS:443 for the
     inference-router, mesh, relay) — no user-defined egress
   - the CR is stamped Degraded
   - the sandbox pod is **not** deployed
   - the controller requeues with backoff

Transient errors (`FetchError::Transient`) preserve the prior
conditions verbatim and re-use the prior LKG (if any). A network blip
cannot collapse a working sandbox.

The LKG cache is **in-process only** — controller restart drops it
deliberately so the first post-restart reconcile of a verify-failing
sandbox cannot ride a stale allowlist across an operator-visible
event. See
`docs/security-audits/2026-04-30-phase2-s12-e-authoritative.md` for
the full threat model.

The `AZURECLAW_FEATURE_SIGNED_ALLOWLIST` env gate from S12.b is
**lifted**; setting or unsetting it has no effect.
