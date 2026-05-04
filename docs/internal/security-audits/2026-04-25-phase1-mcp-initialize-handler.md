# Security Audit: `phase1/mcp-initialize-handler`

**Capability:** real, working MCP `initialize` JSON-RPC method
handler. Pure synchronous function — the entire business logic of
the lifecycle's first request. The future POST `/mcp` axum handler
is now a thin transport wrapper around this function.

## 1. Summary

- New `inference-router/src/mcp/initialize.rs` (≈ 460 lines incl.
  tests). Public API:
  - `handle_initialize(&Request, &InitializeConfig, &dyn SessionMinter) → InitializeOutcome`
  - `OsRngSessionMinter` — production minter (32-byte CSPRNG + hex)
  - `SessionMinter` trait for deterministic test injection
  - `InitializeConfig`, `ServerInfo`, `ServerCapabilities`, `InitializeOutcome` types
- Implements MCP 2025-03-26 lifecycle spec:
  - Negotiates `protocolVersion` — echoes client's if supported,
    otherwise returns server's newest (per spec).
  - Returns `serverInfo`, `capabilities`, optional `instructions`.
  - Mints an `Mcp-Session-Id` from `OsRng` (32 bytes / 256-bit) base16.
- Total function — every input yields a valid `InitializeOutcome`,
  no `unwrap`/`expect` on user-supplied data, no panics.

## 2. Threat model

`initialize` is the MCP entry point and the first byte after a TLS
handshake. It runs pre-auth in the same sense as a TLS Client Hello —
budget bounding and total-function discipline are mandatory.

- **Garbage params (e.g. `params: "string"`)** — rejected with
  structured `InvalidParams` (-32602). Test:
  `rejects_garbage_params_shape`.
- **Missing params** — same. Test: `rejects_missing_params`.
- **Empty `protocolVersion`** — same. Test:
  `rejects_empty_protocol_version`.
- **Unknown `protocolVersion`** — server returns its newest version;
  client decides whether to disconnect (per spec). Test:
  `negotiates_to_newest_when_client_version_unknown`.
- **Wrong method routed here** — `MethodNotFound` (-32601). Test:
  `rejects_non_initialize_method`.
- **Server misconfiguration (no supported versions)** —
  `InternalError` (-32603) with diagnostic data. Test:
  `empty_supported_versions_returns_internal_error`.
- **Forward-compat extra fields** — silently ignored, not rejected.
  Test: `unknown_extra_params_fields_are_tolerated`.

### Session id minting

Production session ids are 32 bytes from `rand::rng()` (re-exports
`OsRng` on Unix — same crate already used by `handoff::token` and
`handoff::pending`), then base16-encoded. Properties:

- 256-bit entropy — exceeds spec's "globally unique and
  cryptographically secure" requirement.
- Lowercase hex output — visible ASCII only — `SessionId::try_new`
  cannot fail. Tests `os_rng_minter_produces_64_char_hex` and
  `os_rng_minter_produces_distinct_ids` lock these invariants.
- Hex chosen over base64url to avoid edge-case wire-format
  collisions; collisions over a 256-bit space are negligible.

The minter is a trait so unit tests inject deterministic ids
(`FixedMinter`). Production code passes `OsRngSessionMinter`.

### Pure-function discipline

`handle_initialize` is synchronous and has no I/O. The future POST
`/mcp` route handler will:
1. Parse the JSON-RPC frame via `mcp::parse_frame`.
2. Validate the `Accept` header via `mcp::validate_accept_header`.
3. Reject bodies > `MAX_FRAME_BYTES`.
4. Dispatch on method — `initialize` calls this function; other
   methods route to their own handlers.
5. Write the response + `Mcp-Session-Id` header.

Step 4's calls are the only place where state-bearing async work
happens; `initialize` itself is pure, which makes both fuzzing
and review tractable.

## 3. Tests

- 15 new unit tests in `mcp::initialize::tests` covering:
  happy path, wrong method, missing params, garbage params shape,
  empty protocol version, unknown version negotiation, supported
  version echoing, instructions inclusion/omission, capabilities
  camelCase, id preservation, empty supported_versions misconfig,
  OsRng hex shape, OsRng uniqueness, forward-compat tolerance.
- 310 router lib tests pass (was 295 — +15).
- `cargo clippy --all-targets -- -D warnings` clean.
- All 7 CI gates green.

## 4. Sign-off

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
