# Security Audit: `phase1/mesh-seam-clarification`

**Capability:** scope-correction PR. Restates the four-seam architecture so
the `MeshProvider` contract is documented as **plugin-side only** (no
router-side `impl`), aligning the trait file and the implementation plan
with how E2E encryption actually works in AzureClaw today and is intended
to work in the future.

**Type:** documentation + doc-comment changes. **No production code paths
are added, removed, or modified.**

## 1. Summary

Prior to this PR, `inference-router/src/providers/mesh.rs` and
internal Phase 1 plan described `MeshProvider` as one of "four
contracts" the router would implement, alongside `PolicyDecisionProvider`,
`AuditSink`, and `SigningProvider`. That framing was a category error.

E2E encryption (Signal Protocol — X3DH + Double Ratchet) runs in the
**agent** process inside the sandbox: today, the OpenClaw plugin
(`mesh-plugin/`) plus the vendored `@agentmesh/sdk`. The router is a
**proxy** for mesh traffic — it forwards the relay WebSocket and registry
HTTPS calls and applies policy/audit hooks around them, but it never holds
keys, never sees plaintext, and must never participate in agent-to-agent
sessions. Putting an `impl MeshProvider for <something>` in the router
would break that invariant.

This PR re-anchors the documentation:

- `providers/mesh.rs` doc-comment now states clearly: plugin-side
  contract; no router impl exists, and none should. Trait still ships as
  documentation of the cross-language contract and as the shape the
  conformance corpus targets.
- `providers/mod.rs` four-seam header now states: three router-side
  contracts (`PolicyDecisionProvider`, `AuditSink`, `SigningProvider`) +
  one plugin-side contract (`MeshProvider`).
- internal Phase 1 plan §1.2 rewrites the "four contracts"
  section, adding §1.2.1 explaining why `MeshProvider` is plugin-side.
  §1.4 adjusts the `spec.agt.providers` schema to drop the misleading
  `mesh: vendored|agt` row (mesh provider selection is plugin-side).
  Phase 0 scope and ordering re-stated. LOC-budget row for
  `controller/src/mesh_peer.rs` rewords "Pull `MeshProvider` out" to the
  accurate "split relay/registry HTTP forwarding + policy-hook glue".

No code logic changes. No traits added, removed, or modified. No `impl`
added or removed. Module re-exports unchanged.

## 2. Threat model delta

**None.** The router's actual behaviour is unchanged. The threat model
(`docs/threat-model.md`) already assumes the router is outside the
agent-to-agent E2E trust boundary — see "Adversary model: malicious or
compromised inference router" assumptions therein. This PR aligns the
*planning documentation* with that assumption; the running system was
already correct.

In particular, the previous wording could have invited a future engineer
to write `impl MeshProvider for Governance` (or similar) in the router,
which **would** have introduced a new attack surface (router holds
ratchet state → router compromise → agent-to-agent confidentiality loss).
This PR closes that footgun by making the architectural rule explicit and
machine-readable in the trait file's doc-comment.

## 3. OWASP mapping

- **OWASP LLM Top 10 v2.0 — LLM07 Insecure Plugin Design:** the router
  is treated as adversarial relative to E2E sessions. This PR strengthens
  that posture by codifying that the router does not (and cannot) become
  a session participant.
- **OWASP MCP Top 10 — MCP05 Excessive Trust Boundaries:** explicitly
  documents that the trust boundary for agent-to-agent messages excludes
  the router, even though the router transports the bytes.

## 4. AuthN / AuthZ path

Unchanged. Mesh authentication remains:

- Agent identity = Ed25519 keypair held in the sandbox.
- Registration with `agentmesh-registry` is signed by the agent.
- KNOCK / X3DH happens between agent processes; router cannot decrypt
  the resulting ciphertext.
- Router's role around mesh remains the existing `PolicyDecisionProvider`
  / `AuditSink` checks on relay/registry proxying — e.g. tenant-level
  authorization to open a relay session, audit records of session metadata
  (size, direction, peer ID, never plaintext).

Outage modes (`Strict` / `CachedRead` / `DegradedDev`) apply to the
router-side seams. Mesh outage handling (relay disconnect, etc.) is
already implemented in the plugin and is unchanged.

## 5. Secret + key custody

Unchanged. **Reaffirmed:** peer identity keys and ratchet state live in
the sandbox plugin. The router holds:

- Workload-Identity-derived Foundry/Entra access tokens (already audited).
- Its own signing keypair for `SigningProvider` (per agent DID, already
  audited under `phase1/signing-provider-in-tree`).

The router does **not** hold and must never hold:

- Peer identity Ed25519 keys.
- Per-session X3DH ephemeral keys.
- Double-Ratchet root/chain keys.
- Per-message keys.

This PR's documentation makes that custody rule explicit at the trait
file. `ci/no-custom-crypto.sh` (§4.4) continues to forbid Signal-protocol
crypto imports outside `vendor/` and the explicitly listed providers.

## 6. Egress surface delta

None. No new outbound destinations. No new TCP/UDP/HTTP endpoints. No
DNS or IP allow-list changes.

## 7. Audit events emitted

None added or removed. Existing relay/registry proxy paths in
`routes/mesh.rs` continue to emit metadata-only events through the
existing `AuditSink` (already implemented behind the trait in
`phase1/audit-sink-in-tree`).

## 8. Failure mode

No new failure paths. Reaffirmed:

- Relay WebSocket disconnect → plugin retries (transport-layer concern).
- Registry HTTPS error → router surfaces 5xx; plugin handles per AGT SDK.
- Router process restart → no mesh-session state lost (it never had any).

## 9. Negative-test coverage

No code-path additions, so no new positive/negative tests are needed.
Existing conformance corpus (`tests/conformance/` Signal/X3DH/ratchet
fixtures, planned per Phase 0 §5.4) targets the *plugin* implementation.
This PR does not change what those fixtures must verify, only clarifies
where the implementation under test lives. Existing 376 workspace tests
remain green.

## 10. Vendored / third-party dependency delta

None. No new crates, npm packages, or vendored sources. The vendored
mesh stack (`vendor/agentmesh-sdk/`, `vendor/agentmesh-relay/`,
`vendor/agentmesh-registry/`) is untouched and continues to provide the
plugin-side mesh implementation.

## 11. Sources / verification (§0.2 #10)

- Repo: `inference-router/src/forward_proxy.rs`,
  `inference-router/src/routes/mesh.rs` — confirmed router only forwards
  opaque WebSocket frames + HTTPS; no decryption code path exists.
- Repo: `mesh-plugin/src/connection.ts`, `vendor/agentmesh-sdk/dist/` —
  confirmed agent-side X3DH / Double Ratchet implementation lives here.
- Repo: `.github/skills/agt-e2e-encryption/SKILL.md` — confirms the
  documented protocol flow ("Router proxy: Inference router proxies
  /agt/relay (WS) and /agt/registry/* (HTTP)") matches this PR's framing.
- Repo: `docs/threat-model.md` — confirmed router is treated as outside
  the agent-to-agent trust boundary.
- User correction (in-session): "the reason why mesh is in TS because
  the openclaw plugin / openclaw or any other agent in plugin will able
  to communicate with each other using end to end encryption — router
  was so far only forwarding packets towards relay and back". This PR
  encodes that statement.

## 12. Sign-offs

Signed-off-by: GitHub Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
