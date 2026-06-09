# mesh-roundtrip-hermes — Hermes Act 2 mesh end-to-end validation

Smallest possible scenario that exercises the **Python AGT MeshClient**
(`kars-agt-mesh`) end-to-end through the full Hermes runtime stack.

## What it proves

Two independent Hermes sandboxes do a bidirectional E2E-encrypted
mesh round-trip, both reaching the AGT relay+registry through the
inference-router proxy (`127.0.0.1:8443/agt/...`) — the same path
the OpenClaw plugin takes via `routerUrl("/agt/registry")`.

Pod B (`mesh-pong-hermes`) runs a long-lived Python echo daemon that
auto-listens on the mesh and replies to every inbound message.

Pod A (`mesh-ping-hermes`) is driven by `hermes -z` (one-shot agent
mode) with a prompt that uses the LLM to call the **real
kars_mesh_send + kars_mesh_await tools** (no harness shortcuts, no
hand-rolled Python on the sender side).

The whole flow:

1. Pod B starts daemon → registers DID → opens relay WS → uploads prekey bundle
2. Pod A's LLM picks `kars_mesh_send(to="mesh-pong-hermes", payload="…")`
3. Hermes plugin's `mesh.py` lazy-inits a MeshClient → registers Pod A's DID
4. MeshClient resolves "mesh-pong-hermes" → DID via `/v1/discover`
5. Fetches B's prekey bundle, runs X3DH, sends `{type:"knock", establishment, ciphertext}`
6. Pod B's `_handle_knock_frame` auto-accepts → `SecureChannel.create_receiver`
7. Decrypts plaintext, daemon prepends `echo(<name>): ` → encrypts reply
8. Pod A's LLM picks `kars_mesh_await(senders=["mesh-pong-hermes"])`
9. Reply arrives, plugin decodes base64 → returns to LLM
10. LLM reports the decoded plaintext prefixed with `RECEIVED:`

Verification: the parent's `transcript.log` must contain
`RECEIVED:echo(mesh-pong-hermes): hello-from-ping`.

## Why this scenario exists separately from `exec-brief-hermes-single`

`exec-brief-hermes-single` was Act 1 (mesh stubs, single agent doing
the whole pipeline). This scenario is the **mesh proof-point** for
Act 2: minimal LLM work, all crypto + transport, no Foundry deps
beyond the model call itself.

## Files

- `manifests/` — two KarsSandboxes (mesh-ping + mesh-pong), each
  with their own InferencePolicy + ToolPolicy.
- `prompt.txt` — driver prompt for `hermes -z` on mesh-ping.
- `daemon.py` — echo daemon source, copied into mesh-pong before
  the prompt fires.
- `config.sh` — drives `daemon.py` into mesh-pong, then posts the
  prompt to mesh-ping via `kubectl exec -c agent -- hermes -z`.

## Run

```bash
cd tools/e2e-harness
SCENARIO=mesh-roundtrip-hermes PLATFORM=local-k8s SKIP_DEV_BRINGUP=1 ./run.sh
```

Watchdog: 240s (most of which is the LLM warmup; the actual mesh
round-trip completes in <1s once both sides are connected).
