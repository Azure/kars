# Behavioral Conformance Corpus

Protocol invariants beyond happy-path — the net that catches
"endpoint returns 200 but never called the crypto step" bugs.
See internal Phase 1 plan §5.4 for the principle statement and
the per-phase corpus coverage table.

## Corpora (per phase)

| Corpus | Phase | Spec file | Fixtures | Status |
|---|---|---|---|---|
| Signal / X3DH / Double-Ratchet | 0 | `specs/signal-x3dh.spec.ts` | `fixtures/signal/` | scaffold (invariants declared as `it.todo`) |
| Signal — KNOCK sequence | 0 | `specs/signal-knock.spec.ts` | `fixtures/signal/` | scaffold |
| Signal — tamper/replay | 0 | `specs/signal-negative.spec.ts` | `fixtures/signal/` | scaffold |
| OAuth 2.1 + PKCE | 1 | `specs/oauth-2.1.spec.ts` | `fixtures/oauth/` | not started |
| MCP 2026 Streamable HTTP | 1 | `specs/mcp-2026.spec.ts` | `fixtures/mcp/` | not started |
| A2A 1.2 Signed Agent Card | 1 | `specs/a2a-1.2.spec.ts` | `fixtures/a2a/` | not started |
| AP2 commerce | 1 | `specs/ap2.spec.ts` | `fixtures/ap2/` | not started |
| Seccomp / Landlock / egress-guard | 0 | `specs/sandbox-isolation.spec.ts` | `fixtures/isolation/` | scaffold (e2e — requires Kind) |
| Cosign / SLSA | 3 | `specs/supply-chain.spec.ts` | `fixtures/supply-chain/` | not started |

## Invariant discipline

Every new protocol route, admission policy, or crypto-adjacent function
ships with at least one positive and one negative test in this corpus
**in the same PR** (plan §5.4).

## Running

```bash
cd tests/conformance
npm ci
npm test
```

E2E-class tests (sandbox-isolation, supply-chain) require a running
Kind cluster and are skipped in local `npm test` unless
`CONFORMANCE_E2E=1` is set. See `harness/kind.ts` (Phase 1).

## Provider axis

From Phase 1 onwards every spec runs across every provider
configuration permitted by its phase (plan §11.3) — at minimum
`{vendored}` and `{agt-policy+audit+signing}` today; `{agt-mesh}` joins
when AGT delivers relay/registry.

## Status of scaffolds

The Phase 0 specs are **scaffolds**: the invariants are declared via
`describe` / `it.todo` and `expect.fail("not yet wired — see plan
§5.4")` so the intent is visible but no false-green signal is produced.
Vitest reports `todo` clearly. Principle §0.2 #8 ("solid, not
look-alike") is upheld: `it.todo` is a documented pending test, not a
silently-passing no-op. Filling in each `todo` happens in the PR that
lands the corresponding implementation (plan §5.4 last paragraph).
