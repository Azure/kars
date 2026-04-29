# Phase 2 — S12.c: CLI `--sign` flag (egress allowlist artifact producer)

**Date:** 2026-04-30
**Slice:** S12.c (`phase2-s12-c-egress-sign`)
**Scope:** CLI-only change. Adds an opt-in `--sign` flag family to
`azureclaw egress` that seals the current `allowedEndpoints` list as a
canonical, content-addressed, cosign-signed OCI artifact and patches
`ClawSandbox.spec.networkPolicy.allowlistRef`. **No** controller,
router, or sandbox-image change.

## Existing implementation surveyed

- `controller/src/crd.rs` — `OciArtifactRef` + `NetworkPolicyConfig.allowlist_ref` (S12.a).
- `deploy/helm/azureclaw/templates/crd.yaml` — `allowlistRef` schema with required-field validation + digest regex (S12.a).
- `docs/policy-canonical-format.md` — byte-stable v1 canonicalization rules (S12.a).
- `cli/src/commands/egress.ts` — pre-existing `--learn`/`--enforce`/`--approve`/`--allowlist`/`--pending` surface; subprocess invocation pattern (kubectl + docker + curl-via-exec).

## Trust model summary

Per `plan.md` "Trust model" for S12:

- The cluster trusts cosign-signed canonical egress allowlist artifacts whose Fulcio identity matches the cluster `SignerPolicy` (S12.d).
- Until S12.d ships, no identity is authoritative — verification result is `AllowlistVerified=Unknown / NoSignerPolicy`.
- The CLI is one possible producer; another would be a CI pipeline using GitHub Actions OIDC. Both produce the same canonical bytes (per S12.a) and use the same `cosign sign` invocation against the same digest.
- The on-CR `allowlistRef` is **non-authoritative** until S12.e flips authority. Operators can review the inline `allowedEndpoints` field (which the producer keeps byte-equivalent to the artifact) and the controller still derives behavior from it.

## Threat surfaces introduced

This slice introduces three subprocess invocations and one new place that touches an OIDC token:

1. **`oras push` invocation** — uploads canonical YAML bytes to a registry. Risk: an attacker who controls the CLI invocation could push to a registry of their choice. Mitigated by passing `--registry` as an explicit argument (no shell interpolation, no env-var fallback that reads from untrusted sources).
2. **`cosign sign` invocation** — issues a signature over the digest. Risk: mode confusion (e.g., signing with a key when the operator intended keyless). Mitigated by explicit `--sign-mode` flag and an auto-detect rule that errs on the side of failing closed (non-TTY without token and without key → hard error, not silent fallback).
3. **`kubectl patch` invocation** — writes `allowlistRef` to the live `ClawSandbox`. Risk: orphan ref if signing failed but patch succeeded. Mitigated by ordering: patch is the last step, executed only after both push and sign succeed; any earlier failure aborts before patching.
4. **OIDC token handling** — `--sign-mode identity-token` reads `SIGSTORE_ID_TOKEN` or `OIDC_TOKEN` from env and forwards it via cosign's `--identity-token` flag. The token never appears in argv for any other binary; it is not logged.

## Mitigations

- **No shell interpolation.** All subprocess invocations use `execa(file, argv)` with structured argv arrays. No template strings, no `sh -c`. Verified by tests `pushArtifact`, `signArtifact`, `patchClawSandbox` that lock in the exact argv shape.
- **Argv pass-through only.** The `--registry`, `--repository`, `--sign-key` flag values are passed directly into argv positions; no concatenation into a single string that a downstream shell could re-parse.
- **Tool detection before exec.** `ensureSigningTools` looks up both binaries via `which` before any push/sign attempt. Missing-binary errors include the upstream install URL (`https://oras.land/docs/installation` and `https://docs.sigstore.dev/cosign/installation`).
- **Producer-side digest verification.** The CLI computes `sha256` over the canonical bytes locally and compares to the digest reported by `oras push`. Mismatch is treated as a producer bug per S12.a §"Signing" and aborts before `cosign sign`.
- **Idempotent `kubectl patch`.** The patch is a JSON merge patch over `spec.networkPolicy.allowlistRef`; re-running with the same digest is a no-op. The inline `allowedEndpoints` is left untouched so the resource remains a valid pre-S12.e ClawSandbox.
- **Fail-closed ordering.** Push → sign → patch. Any failure in steps 1–2 aborts before step 3. No orphan `allowlistRef` ever lands on a `ClawSandbox` from this CLI.
- **Strict canonicalization.** The hand-rolled YAML emitter rejects uppercase ASCII, leading/trailing whitespace, control bytes, wildcards, dotted-zero ports, generation ≤ 0, and any byte outside `[a-z0-9.-]` after IDNA-2008 conversion. Tests lock in each rejection rule.
- **Token never logged.** `signArtifact` does not echo the identity token; cosign itself handles redaction.

## Why ref still non-authoritative

Per `plan.md` §S12 decomposition:

- S12.b validates the artifact's canonical bytes and surfaces `AllowlistVerified` as a status condition only.
- S12.d pins authoritative identity via `SignerPolicy`.
- S12.e flips the controller to derive `allowedEndpoints` from the verified canonical bytes.

Until S12.e, the inline `allowedEndpoints` field is what drives sandbox behavior. The CLI in S12.c keeps both representations byte-equivalent: it mutates the live spec via `--enforce`/`--approve` (existing surface), then reads back the mutated state, builds canonical bytes from it, and patches `allowlistRef` to the resulting digest. Operators can review the inline list as the human-readable source of truth without ambiguity about what was sealed.

## Test coverage

41 new tests across two files:

- `cli/src/commands/egress/sign.test.ts` (34 tests) — canonical YAML serializer (sort, dedupe, IDNA, port-explicit, byte-stable, rejections); `digestOfCanonical` digest format; `autoDetectSignMode` (TTY/keyless, identity-token, keyed, error cases); `ensureSigningTools` (success + each missing-binary error); `buildOrasPushArgv` / `parseOrasDigest` / `pushArtifact` (digest-mismatch abort); `buildCosignSignArgv` per mode + keyed-without-key rejection; `signArtifact`; `buildPatchArgv` / `patchClawSandbox` argv shape.
- `cli/src/commands/egress.test.ts` (7 tests) — flag wiring (`--sign`, `--no-sign`, `--sign-mode`, `--sign-key`, `--registry`, `--repository`); `--sign` without `--enforce`/`--approve` errors and sets `process.exitCode = 1`.

CLI test count 354 → 395 passing (2 pre-existing skipped). `npx tsc --noEmit` clean. `npm run lint` introduces no new warnings in egress files. `npm run build` green.

## §10.5 Compliance

- §10.5 #4 (signed-policy provenance): producer side now ships. Combined with S12.b's consumer-side validation, the cluster has end-to-end coverage of "the bytes the operator sealed are the bytes the controller verifies" — modulo authority pinning, which lands in S12.d.
- Per-sub-slice audit doc: this is the third (S12.a, S12.b, S12.c).

## References

- Plan §S12.c: `~/.copilot/session-state/13bea069-bbc9-48ae-a25c-36da81c7a0fe/plan.md`
- Canonical format: `docs/policy-canonical-format.md`
- ORAS install: <https://oras.land/docs/installation>
- Cosign install: <https://docs.sigstore.dev/cosign/installation>
- Sigstore keyless (Fulcio): <https://docs.sigstore.dev/cosign/keyless/>

## Sign-offs

- [ ] Author: ____________________
- [ ] Independent reviewer: ____________________
