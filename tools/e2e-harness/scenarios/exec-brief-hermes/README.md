# exec-brief-hermes — multi-agent Hermes mesh e2e

Parent Hermes sandbox uses `kars_spawn` to launch 3 Hermes sub-agents
(`analyst`, `viz`, `writer`) and coordinates them via the real
**Python AGT MeshClient** (`kars-agt-mesh`) — the Act 2 Hermes mesh
path that replaced the Act 1 `Mesh communication not available` stubs.

This is the Hermes parallel of the canonical OpenClaw `exec-brief`
scenario.

## What it proves

1. Hermes parent spawns Hermes children (NOT OpenClaw children).
   Fix landed in `inference-router/src/spawn/mod.rs`: the router now
   reads `KARS_RUNTIME_KIND` from its own env (controller injects this
   on every v1 runtime container) and stamps every child CRD with
   the same runtime kind. The legacy code hard-coded `OpenClaw`.
2. The Hermes mesh plugin (`runtimes/hermes/.../plugin/mesh.py`)
   accepts the OpenClaw-style `to_agent`+`content` arg naming so a
   prompt that was written for OpenClaw mesh works on Hermes too.
3. The 6-tool deny list (delegate_task / mixture_of_agents / cronjob /
   kanban_create / kanban_comment / send_message) is enforced — the
   parent has zero way to spawn off-cluster agents.
4. The kars_spawn → kars_mesh_send → kars_mesh_await coordination
   loop works end-to-end with real Foundry tool calls in each
   sub-agent.

## Run

```bash
cd tools/e2e-harness
SCENARIO=exec-brief-hermes PLATFORM=local-k8s SKIP_DEV_BRINGUP=1 ./run.sh
```

Watchdog: 2400s. Most of it is the Foundry calls (web_search,
code_execute, image_generate); the mesh transport itself is sub-second.
