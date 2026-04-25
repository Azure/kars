# Security audit — A2A trust-store snapshot rebuild orchestrator

**Date:** 2026-04-25
**PR branch:** `phase1/trust-store-rebuild-orchestrator`
**Capability owner:** AzureClaw Phase 1 — A2A trust path

## 1. Summary

Adds `inference-router/src/a2a/snapshot_rebuild.rs` — a pure function
`rebuild_snapshot(specs, generation, source_prefix) -> RebuildOutcome`
that consolidates many `A2aAgentSpec`s into a single
`TrustStoreSnapshot` ready for `TrustStore::replace_snapshot`.

This is the orchestrator the Phase 2 `A2AAgent` informer reconciler will
call on every event: it accepts the full set of specs observed in the
informer cache, projects each, builds one snapshot, and surfaces a
list of issues for any malformed or conflicting CR.

Three properties locked by the test corpus:

1. **Deterministic ordering** — specs sorted by `(namespace, name)`
   before the builder runs; first-wins resolution is reproducible
   across reconciler restarts.
2. **Fault isolation** — a malformed CR (projection failure) drops
   only its own anchors; valid CRs land in the snapshot.
3. **Conflict resolution** — duplicate kids across CRs do not poison
   the snapshot; the first-seen wins, the conflicting one is dropped
   with a `RebuildIssue::DuplicateKid` for the reconciler to surface
   on the offending CR.

## 2. Threat model delta

### Asset gaining new exposure
None. New code is a pure function; not yet wired into a reconciler.

### STRIDE delta vs `docs/threat-model.md`

- **Tampering (T)** — a malformed `A2aAgent` CR cannot brick the trust
  store: `projection_failure_drops_offending_spec_only` asserts the
  good CRs still land. Without this, an adversary controlling one
  CR could DoS the entire A2A trust path by submitting an unparsable
  spec.
- **Spoofing (S)** — a second CR cannot silently shadow a kid already
  contributed by a first CR: `conflict_kid_first_seen_wins_with_issue_emitted`
  asserts the conflict is rejected and surfaced. Lexicographic
  `(namespace, name)` ordering ensures attackers can't win the race
  by getting reconciled first via timing tricks; the loser is
  whichever CR sorts later.
- **Repudiation (R)** — every dropped/conflicting CR is enumerated in
  the returned `Vec<RebuildIssue>`, ready for the reconciler to
  publish on the CR's `status.conditions[*]`. Operator visibility is
  preserved.

## 3. OWASP mapping

- **OWASP MCP05 — Authentication and Authorization Bypass:** the
  first-wins conflict policy denies a later CR the ability to redirect
  trust by overwriting an earlier CR's `kid`.
- **OWASP LLM07 — Insecure Plugin Design:** the malformed-CR test
  asserts blast-radius isolation — one bad spec does not knock out
  the whole trust path.

## 4. AuthN / AuthZ path

Not applicable — pure orchestrator, no I/O. The function consumes
`A2aAgentSpec`s already validated by K8s admission (CEL, schema) and
returns an in-memory snapshot. K8s API authn/authz protects the
upstream CR write; this code is downstream of that gate.

## 5. Secret + key custody

No secrets handled. Public keys (Ed25519 verifying keys) only.

## 6. Egress surface delta

None. Pure function.

## 7. Audit events emitted

None directly. The reconciler that calls this function (Phase 2)
will:

- Emit one K8s `Event` per `RebuildIssue` against the offending CR.
- Update `status.conditions[*]` so `kubectl get a2aagent` shows the
  problem.

That wiring is out of scope for this PR; the `RebuildIssue` enum is
the contract.

## 8. Failure mode

Fail-open with isolation:

| Input | Behaviour |
|---|---|
| Empty spec list | empty snapshot, no issues |
| All specs valid, no kid conflicts | full snapshot, no issues |
| One spec malformed (projection error) | other specs land; offending one dropped with `Projection` issue |
| Two specs claim same kid | first-seen (lex order) wins; second dropped with `DuplicateKid` issue |
| One spec lists same kid twice (in-CR duplicate) | projection rejects the entire spec; `Projection` issue emitted |

The "fail-open with isolation" choice is deliberate: a single
malformed CR must not delete the entire trust map, because doing so
would cause every in-flight A2A handshake to fail simultaneously.

## 9. Negative-test coverage

Seven in-tree tests in `inference-router/src/a2a/snapshot_rebuild.rs`:

- `empty_input_yields_empty_snapshot_no_issues`
- `many_specs_with_unique_kids_all_land_in_snapshot`
- `conflicting_kid_first_seen_wins_with_issue_emitted`
- `projection_failure_drops_offending_spec_only`
- `duplicate_anchor_within_same_owner_is_silently_deduped`
- `deterministic_ordering_independent_of_input_order`
- `generation_round_trips`

The determinism test in particular is a regression bar against any
future change that switches to `HashMap` iteration order or skips
the explicit sort.

## 10. Vendored / third-party dependency delta

None. Reuses workspace dependencies (`thiserror`, `ed25519-dalek`,
`base64`).

Sources consulted:

- `inference-router/src/a2a/agent_projection.rs:129-180`
  (`project_anchors` contract).
- `inference-router/src/a2a/trust_store.rs:176-223`
  (`TrustStoreBuilder::add` semantics for duplicate vs identical
  re-adds).
- PR 33 audit doc
  (`2026-04-25-phase1-trust-store-hot-reload-integration.md`) for
  the integration-level invariants this rebuild function preserves.

## 11. Sign-offs

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
