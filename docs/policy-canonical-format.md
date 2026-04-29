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
2. **Authority:** signer identity (Fulcio cert SAN + issuer claim) matches an entry in the cluster `SignerPolicy` ConfigMap (S12.d). Without S12.d, no identity is authoritative — verification is "valid sig" only and the controller MUST surface this as a `AllowlistVerified=Unknown` condition with reason `NoSignerPolicy`.

After verification, parse the YAML and revalidate every canonicalization rule above. Reject (set `AllowlistVerified=False`, reason `CanonicalFormViolation`) on any mismatch — even a valid signature over malformed canonical bytes is not authority.

## Forward compatibility

When v2 is needed (e.g., to reintroduce wildcards, methods, paths, CIDR ranges):

- Bump artifactType to `application/vnd.azureclaw.egress-allowlist.v2+yaml`.
- Define new canonicalization rules. v2 consumers SHOULD also accept v1 artifacts; v1 consumers MUST reject v2 artifacts (forward incompatibility is the safe direction).
- The on-CR `OciArtifactRef.artifactType` is the authority — controllers select the canonical-format codec by media-type, not by `apiVersion` field inside the document.

## Status

| Slice | Owns |
|---|---|
| **S12.a** *(this slice)* | Format definition, CRD field, no behavior |
| S12.b | Controller fetcher + format validator (status-only) |
| S12.c | CLI canonical serializer + signing |
| S12.d | `SignerPolicy` ConfigMap + identity-pinned authority |
| S12.e | Authoritative ref mode (controller derives `allowedEndpoints` from canonical bytes) |
