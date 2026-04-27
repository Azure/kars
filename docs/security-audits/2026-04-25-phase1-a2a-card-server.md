# Security Audit: `phase1/a2a-card-server`

**Capability:** server-side AgentCard build pipeline. Pure synchronous
function transforming a declarative `AgentCardConfig` + `SigningKey`
into the JSON bytes the future GET `/.well-known/agent.json` route
will serve.

## 1. Summary

- New `inference-router/src/a2a/card_server.rs` (≈ 380 lines incl. tests).
- `build_signed_card(config, signing_key) → Vec<u8>` — happy path produces a fully populated, EdDSA-signed AgentCard ready to serve.
- `build_card(config) → AgentCard` — unsigned variant for callers that need to add extra interfaces or co-signatures before sealing.
- `AgentCardConfig` is owned-string only (no lifetime gymnastics) — designed for projection from `ClawSandbox.spec.a2a.*` or env vars at sandbox startup.
- Mirrors the symmetry with `mcp::pipeline::process_request`: pure, total, no I/O, fully tested.

## 2. Threat model

| Threat | Mitigation | Test |
|---|---|---|
| Empty name → registry collision | `EmptyName` error refuses build before signing | `build_card_rejects_empty_name` |
| Skill-less card → useless but valid wire shape | `NoSkills` error refuses build | `build_card_rejects_no_skills` |
| snake_case leaking to wire (would break interop with reference impls) | Regression test asserts `supportedInterfaces`/`defaultInputModes`/`protocolVersion`/`protocolBinding` camelCase + asserts `supported_interfaces`/`default_input_modes` are absent | `build_signed_card_emits_camel_case_wire_form` |
| Unset optionals leaking as `null` (changes hash, breaks signature verify on receivers) | All optional fields use `skip_serializing_if = "Option::is_none"`; test asserts JSON omits documentationUrl / iconUrl / provider / streaming / pushNotifications when None | `build_signed_card_omits_unset_optionals` |
| A2A protocol version drift | Pinned to `"1.0"`; regression test asserts the literal string in the wire bytes | `build_signed_card_protocol_version_pinned` |
| Tampering after signing | Round-trip + tamper test confirms verify rejects mutated card under same trust anchor | `signed_card_tamper_breaks_verify` |
| Wrong-`kid` impersonation | Signed-with-A, trusted-with-B-on-A's-kid path rejected | `signed_card_signed_with_different_key_rejected` |
| Unknown-`kid` (key rotation accident) | Trusted key under a different `kid` → no match | `signed_card_with_wrong_kid_does_not_verify` |
| Multi-signer co-sign workflow | Sign-then-cosign produces a 2-sig card; verify works under either trust anchor independently | `signed_card_signatures_are_appendable` |
| Default-mode override correctness | Override path produces overridden modes; default produces `["text/plain"]` | `build_card_default_modes_overridable`, `build_card_happy_path` |

### Failure mode

All input that would produce a malformed card surfaces as `CardServerError` at build time, **before** the route ever serves a byte. There is no path that produces a corrupt-but-200 response. This is the symmetric guarantee to `mcp::pipeline::process_request`.

### What this layer DOES NOT do

- Does not bind a route. The future `/.well-known/agent.json` GET handler is a 3-line wrapper that calls `build_signed_card` once at startup, caches the bytes, and serves them with `Content-Type: application/json`.
- Does not fetch the signing key. Key custody stays with `SigningProvider` (AGT-backed in prod, vendored in dev).
- Does not load `AgentCardConfig` from any source. Future PRs wire the projection from `ClawSandbox.spec.a2a` or `McpServer.spec` as appropriate.

## 3. Tests

- 14 new unit tests in `a2a::card_server::tests` — every threat in the table covered.
- 342 router lib tests pass (was 328 — +14).
- `cargo clippy --all-targets -- -D warnings` clean.
- All 7 CI gates green.

## 4. Sign-off

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
