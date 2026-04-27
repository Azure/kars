# Security Audit — Phase 1: A2A axum routes (`/.well-known/agent.json` + `POST /a2a`)

**Date:** 2026-04-25
**Scope:** `inference-router/src/routes/a2a.rs`, `inference-router/src/routes/mod.rs`
**Branch:** `phase1/a2a-route-binding`

## 1. Summary

Adds the axum binding for A2A 1.0.0:

- `GET /.well-known/agent.json` returns the router's signed Agent Card
  (Ed25519 / EdDSA over the canonical card payload, JWS protected header
  carrying the `kid`).
- `POST /a2a` accepts JSON-RPC 2.0 requests, batches, and notifications.
  Methods bound: `message/send`, `tasks/get`, `tasks/cancel` (the three
  pure handlers landed in PR 22). Unknown methods produce a JSON-RPC
  `-32601` envelope; malformed bodies produce `-32700`; oversized bodies
  produce `413`; incompatible `Accept` produces `406`; notifications
  (no `id`) produce `202 Accepted`.

The route is mounted as a sub-router with its own typed state
(`A2aRouteState` = card config + Ed25519 signing key + task store + id
minter) and is independent of `AppState`. This keeps coupling explicit
and makes handlers `tower::ServiceExt::oneshot`-testable end-to-end
without standing up an `AppState` (which performs network I/O at build).

## 2. Threat model delta

| Asset | New exposure | STRIDE |
|---|---|---|
| Router process memory | Inbound A2A frames now reach JSON-RPC dispatch. | DoS (oversized body) |
| Agent Card signing key | Used per-request to sign the served card. Held in `Arc<SigningKey>`. | Tampering / Information disclosure |
| Task store | `message/send` writes to `InMemoryTaskStore`; `tasks/get`/`tasks/cancel` read/mutate. | Tampering / DoS |

Mitigations (already in pure layer, exercised by these new tests):

- 4 MiB body cap before parse → `413`.
- `Accept` permissive but enforced when present (`*/*`, `application/*`,
  `application/json`, `text/event-stream` accepted; anything else → `406`).
- Card signing happens **per request from the in-process `Arc<SigningKey>`**;
  no key egress; no per-request key rotation (the card is deterministic
  given `(config, key)`, so re-signing on every request is acceptable).
- `InMemoryTaskStore` is a Send+Sync `Mutex<HashMap>`; concurrent access is
  serialised. Persistence/HA is a future PR (`KubeTaskStore`).

## 3. OWASP mapping

OWASP MCP/A2A-adjacent controls:

- **Card forgery (A2A §5.5 trust model).** Card is signed via in-process
  Ed25519 SigningKey; verifiers (the `card_verifier` module + `TrustStore`
  cache) reject tampered/expired/wrong-issuer cards. A regression test
  (`served_card_carries_expected_kid`) confirms the `kid` reaches the
  protected header.
- **Method injection / unbounded dispatch.** Method names are matched by
  exact string equality against the closed set
  `{message/send, tasks/get, tasks/cancel}`; everything else falls through
  to `-32601` with the offending method name surfaced as structured `data`
  (no string interpolation into log lines that an attacker could exploit).
- **Confused-deputy via batch.** Each batch item dispatched independently;
  a `Frame::Response` or nested `Frame::Batch` inside a batch is rejected
  per-item with an `InvalidRequest` envelope; the rest of the batch
  continues.

## 4. AuthN / AuthZ path

- **Today:** none at the route layer. Anonymous callers can hit both
  endpoints. This is acceptable because the route is **not yet wired into
  `main.rs`** — wiring is gated on the OAuth 2.1 / mTLS layer and on
  resolving §6 of internal Phase 1 plan (per-tenant outage mode).
- The card itself is meant to be served unauthenticated (it's a discovery
  document); the `POST /a2a` endpoint will sit behind OAuth 2.1 + card
  verification when wired.

## 5. Secret + key custody

- The `Arc<SigningKey>` is constructed in `A2aRouteState::new` from a
  caller-provided `SigningKey`. In production the wiring code (future PR)
  will source this from the `SigningProvider` trait — i.e. from
  `Governance.identity` (Ed25519 keypair generated at boot,
  in-memory-only, never written to disk by the router itself).
- The signing key never leaves the process. Tests use a fixed seed
  (`[7u8; 32]`) inside `#[cfg(test)]` only.

## 6. Egress surface delta

Zero new outbound destinations. All A2A handlers are in-process today.
Outbound A2A calls (router → other agent's `/a2a`) are a separate code
path (`a2a::card_verifier`) and not introduced by this PR.

## 7. Audit events emitted

None at this layer. When `message/send` is wired into the production
router pipeline, it must emit an `AuditSink::append_with_dedup` event per
the existing pattern in `routes/handoff.rs`. Tracked in the next PR.

## 8. Failure mode

Every path is total and fail-closed:

| Input | Output | Test |
|---|---|---|
| body > 4 MiB | `413` | `oversized_returns_413` |
| `Accept: text/html` | `406` | `incompatible_accept_returns_406` |
| missing `Accept` | accepted | `missing_accept_is_permissive` |
| `Accept: */*` | accepted | `star_slash_star_accepted` |
| malformed JSON | `-32700` | `malformed_json_returns_parse_error` |
| empty batch (parse error) | `-32600` | `empty_batch_returns_invalid_request` |
| unknown method | `-32601` | `unknown_method_returns_method_not_found` |
| `tasks/get` on missing id | `-32001` (TaskNotFound) | `tasks_get_returns_task_not_found_for_unknown` |
| `message/send` happy path | `200` + Submitted task | `message_send_creates_task_and_returns_it` |
| notification only | `202` empty | `notification_returns_202` |
| batch all-notifications | `202` empty | `batch_of_only_notifications_returns_202` |
| mixed batch | per-item responses | `batch_dispatches_each_item` |
| `GET /.well-known/agent.json` | signed card | `get_agent_card_returns_signed_card` |

## 9. Negative-test coverage

15 `#[cfg(test)]` tests cover all paths in §8 plus
`served_card_carries_expected_kid` which confirms the JWS protected
header carries our `kid`. Combined with the AP2 conformance corpus
(PR 25) and the trust-store tests (PR 24) these form the wire-level
A2A Phase 1 surface.

## 10. Vendored / third-party dependency delta

- `axum`, `tower`, `ed25519_dalek`, `serde_json`, `base64` — all already
  workspace deps. No new production dependencies.
- Vendored AGT SDK untouched.

## 11. Sign-offs

The route is not yet mounted into the production router. Before that
mount lands:

1. The `OAuth 2.1` / mTLS tower layer must wrap `POST /a2a`.
2. The `Arc<SigningKey>` must be plumbed from `SigningProvider`
   (Governance) — not from a fresh keypair.
3. `message/send` must emit an `AuditSink::append_with_dedup` event
   carrying the (sandbox, peer, task-id) tuple.

These three follow-ups are tracked in the Phase 1 close-out checklist.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>

## References

- A2A 1.0.0 spec — <https://a2a-protocol.org/v1.0.0/specification>
- JSON-RPC 2.0 — <https://www.jsonrpc.org/specification>
- `inference-router/src/a2a/jsonrpc_dispatch.rs` (PR 22) — pure handlers.
- `inference-router/src/a2a/card_signing.rs` — Ed25519 / EdDSA JWS card signature.
- `inference-router/src/a2a/trust_store.rs` (PR 24) — verifier-side cache.
