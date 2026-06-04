# Security Audit — Hermes Act 2: Python AGT mesh + sub-agent tool deny list

**Date**: 2026-06-04
**Author**: P. Allakatos
**Reviewer**: Copilot (independent critique pass)
**Scope**: PR #396 (branch `hermes/act1-docker-smoke-fixes`)
**Risk class**: Capability-defining (sandbox image change + new outbound mesh path)

## Summary

Replaces Hermes' Act 1 mesh stubs with a real AGT-based encrypted
sub-agent transport, and removes six built-in Hermes sub-agent tools
that would otherwise let the LLM bypass kars' governance perimeter.

Two artefacts ship together:

1. `runtimes/agt-mesh-python/` — new `kars-agt-mesh` Python package
   (~800 LOC). Wraps the upstream `agentmesh-platform` crypto
   primitives (X3DH, Double Ratchet, SecureChannel) into a small
   `MeshClient` that connects to the same AGT relay+registry the rest
   of kars uses. Runtime-neutral so any Python framework can adopt
   it; Hermes is just the first consumer.
2. `runtimes/hermes/src/kars_runtime_hermes/plugin/{__init__.py,
   mesh.py}` — Hermes adapter: registers four governed mesh tools
   (`kars_mesh_send`, `_inbox`, `_await`, `_transfer_file`) and
   deregisters six Hermes built-ins (`delegate_task`,
   `mixture_of_agents`, `cronjob`, `kanban_create`, `kanban_comment`,
   `send_message`).

## Why these tools are denied

| Tool | Threat | Disposition |
|------|--------|-------------|
| `delegate_task` | Spawns child `AIAgent` and opens an outbound ACP connection — bypasses the kars sandbox boundary entirely. | Hard deny. Sub-agent spawn must go through the controller's `kars_spawn` path so the child gets its own NetworkPolicy + seccomp profile. |
| `mixture_of_agents` | Issues direct HTTPS calls to OpenRouter regardless of provider config — the egress-guard then drops them and the agent hangs in a retry loop. Even if egress were allowed it would skip the inference-router's policy stack (Content Safety, token budget, audit). | Hard deny. |
| `cronjob` | Submits a job to Hermes' embedded cron dispatcher (interval=60s, runs inside the gateway process). Each tick re-invokes the agent without a fresh authorization decision, so a single LLM call can schedule unattended runs that escape the user's session. | Hard deny. Recurring tasks must be modelled as a separate `KarsSandbox` with explicit policy. |
| `kanban_create` / `kanban_comment` | Same embedded-dispatcher pattern: the kanban ticker pulls cards and spawns workers without an active turn. Compromises the "every LLM action is approved by the active session" invariant. | Hard deny. |
| `send_message` | A generic POST helper — the LLM picks the URL. With `HTTPS_PROXY` set this is routed through the forward proxy, but the LLM can easily reach intra-cluster targets (registry, controller API) that the proxy doesn't whitelist. | Hard deny. The governed equivalent is `kars_mesh_send`, which is E2E encrypted and policy-checked. |

The deny list is enforced in **two** places (defence in depth):

- Plugin-side: `_HERMES_DENY` calls `ctx.deregister_tool(name)`
  immediately after Hermes' built-in plugin pack registers, so the
  tools are physically absent from the LLM's tool list.
- AGT-profile-side: `tools/e2e-harness/scenarios/exec-brief-hermes-single/
  manifests/02-toolpolicy.yaml` adds a `denied_actions:` block at
  priority 100 referencing the same six tool names. If a future image
  build ever loses the plugin-side guard, AGT itself will refuse the
  call.

## Sandbox image change (`sandbox-images/hermes/Dockerfile`)

Added two lines:

```dockerfile
COPY runtimes/agt-mesh-python/ /opt/kars-agt-mesh/
RUN pip install /opt/kars-agt-mesh
```

This pulls the new `kars-agt-mesh` Python package into the image
before the plugin install stage. The package's only runtime deps
already shipped with the prior image (`agentmesh-platform>=3.6.0`,
`httpx`, `pynacl`, `websockets`), so the net new attack surface is
the ~800 LOC of `kars_agt_mesh/*`. All proof / DID handling is
covered by unit tests (`runtimes/agt-mesh-python/tests/`).

## Threat model — `kars-agt-mesh` library

- **Identity material on disk**: `IdentityStore.load_or_create()`
  writes Ed25519 + X25519 keys to a single JSON file with mode
  0600 inside the sandbox's writeable `/sandbox` mount. Mode is set
  via `os.fchmod` before the first write; verified by unit test.
  Reading another sandbox's identity requires breaking out of the
  pod's filesystem isolation, which is out of scope for the agent's
  threat model.
- **Network egress**: the client never reaches the relay or registry
  directly. It uses the router-proxied URLs
  `http(s)://127.0.0.1:8443/agt/{relay,registry}` so all mesh traffic
  flows through the inference-router's existing audit + rate-limit
  path. The egress-guard iptables rule that drops ports other than
  80/443 stays in place; nothing in this package punches a new hole.
- **Replay safety**: registry POP uses Ed25519 signature over
  `base64url(public_key) || iso_timestamp`. The registry's
  `REPLAY_WINDOW` is 5 minutes. Even if a proof were captured by an
  attacker with control of the relay, they could not register a new
  agent with the same DID (DID is server-derived from
  `sha256(public_key)`).
- **Singleton guard**: `_SINGLETONS` dict keyed on
  `(name, relay_url, registry_url)` mirrors the OpenClaw
  `Symbol.for("agt-mesh-client")` pattern so a process can't end up
  with two `MeshClient` instances racing on the same identity.

## Tests

- `runtimes/agt-mesh-python/tests/`: 9 unit tests (package shape,
  identity round-trip, DID format).
- `runtimes/hermes/tests/`: 83 tests (previously 83 with stubs;
  swapped one stub-specific test for a `mesh_module_is_real_not_stub`
  guard).
- Live verification in kind cluster (`kars-dev`):
  - `MeshClient.connect()` against the router proxy returns
    `201 Created` from the registry and the WS upgrade succeeds.
  - Self-discovery via `/v1/discover?capability=execbrief-hermes`
    returns the agent's own DID.
  - Plugin loader logs the six deregistrations and the four mesh
    tools appear in `plugin_tool_names`.
  - **Full bidirectional round-trip between two sandboxes**: from a
    freshly-built image (no hot patches), pod A (`execbrief-hermes`)
    discovers pod B (`smoke-hermes`) by name, runs X3DH, sends a
    KNOCK + first ciphertext, pod B auto-accepts, decrypts the
    plaintext `b"hello from execbrief-hermes"`, encrypts and replies
    `b"pong from smoke-hermes"`, and pod A decrypts the reply — all
    through the inference-router proxy (`127.0.0.1:8443/agt/...`)
    with egress-guard iptables still in place.

## Residual risks

1. **Persistent identity is per-pod-restart only**: the IdentityStore
   writes to `/sandbox/.agt/identity.json` which the controller mounts
   as an `emptyDir` by default. A pod restart that reuses the volume
   keeps the DID; recreating the pod loses it. Operators wanting
   cross-recreation identity should mount a PVC at `/sandbox/.agt`.
   The Act 2.2 follow-up will plumb this through the `KarsSandbox`
   CRD as an optional `meshIdentity.persistent: true` knob.
2. **Multi-process Hermes**: Hermes' `lazy_install` worker is a
   subprocess, and would need its own MeshClient or a broker. With
   `delegate_task` denied (effectively, because `delegate_task` is
   the only thing that triggers it in practice) this isn't reachable.
   Re-audit if Hermes ever ships a tool that spawns workers outside
   the gateway.
3. **Plugin-side deny is the only enforcement in non-AGT scenarios**.
   If a future scenario YAML forgets the `denied_actions:` block,
   only the plugin guard remains. Mitigated by both layers shipping
   in the same PR and the plugin guard being in the image (harder to
   accidentally bypass than a scenario YAML).
4. **Stale registry entries**: When a sandbox is restarted with a
   different identity (e.g. the emptyDir was wiped), the prior
   registration remains in the registry until its TTL expires.
   `MeshClient.discover()` now sorts by `last_seen` descending so
   the freshest entry is picked first, but offline-then-reborn
   sandboxes will receive traffic addressed to their old DID for
   the TTL window. Not security-impacting (messages can't be
   decrypted without the old private key) but worth knowing for
   debugging.

## Sign-off

Signed-off-by: Pal Allakatos <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
