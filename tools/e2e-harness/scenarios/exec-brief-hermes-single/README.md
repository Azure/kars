# exec-brief-hermes-single — single-agent variant on Hermes

The canonical [`exec-brief`](../exec-brief/) scenario is a four-agent
showcase (parent + analyst + viz + writer) that exercises the
encrypted mesh. This variant collapses it to **one Hermes agent doing
the whole pipeline itself** so we can validate the Hermes runtime
adapter end-to-end without depending on the Python AGT MeshClient
(which ships in Act 2 of the Hermes work; until then,
`kars_mesh_*` returns explicit "Act 2 not ready" errors).

## What it exercises

- `kars_discover('*')` (kars plugin → router → AGT registry)
- `foundry_web_search` × ≥3 (kars plugin → router → Foundry)
- `foundry_memory` upsert (kars plugin → router → Foundry Memory Store
  with kars's `memory-<sandbox>` store-name convention)
- `foundry_code_execute` (matplotlib scorecard)
- `foundry_image_generation` (gpt-image-1 hero)
- `file_write` (Hermes native tool, writes brief.md)
- `pre_tool_call` governance hook (kars plugin → router /agt/evaluate)
- McpServer projection (DeepWiki via Hermes' native MCP client,
  routed through the loopback router with `x-kars-mcp-server` header)
- KarsMemory binding (`memory-execbrief-hermes` storeName)
- ToolPolicy attachment + router-side digest enforcement

## What it does NOT exercise

- `kars_mesh_*` (deferred to Act 2)
- `kars_spawn` (no sub-agents — explicitly told not to in the prompt)
- Inter-agent message handoff
- Channel egress (no Telegram bot configured by default; the run
  delivers the brief by writing `/sandbox/incoming/brief.md` and the
  harness collects it from there)

## Running

Same orchestrator + drivers as exec-brief, just with the scenario
name swapped:

```bash
# Local kind (requires `kars dev --target local-k8s` to have brought
# up a cluster + loaded the kars-runtime-hermes:dev image):
SCENARIO=exec-brief-hermes-single PLATFORM=local-k8s \
  ./tools/e2e-harness/run.sh

# AKS (requires the Hermes runtime image pushed to your ACR;
# defaults to <acr>/kars-runtime-hermes:latest):
SCENARIO=exec-brief-hermes-single PLATFORM=aks \
  ./tools/e2e-harness/run.sh
```

Pass `WATCHDOG_SECS=2400` for slower iterations; `TELEGRAM_BOT_TOKEN=…`
to enable the channel path (the prompt does not currently exercise
it but the manifest accepts the credential).

## verify.py expectations

The shared `verify.py` runs the same 9 acceptance checks as the
canonical scenario, but several are no-ops here:

| Check | Status in this variant |
|---|---|
| `kars_discover` was called | required (Step 1 in prompt) |
| ≥3 `foundry_web_search` calls | required |
| `foundry_memory` upsert with right store name | required |
| `foundry_code_execute` produced scorecard.png | required |
| `foundry_image_generation` produced hero.png | required |
| `file_write` wrote brief.md (~700-800 words) | required |
| `kars_mesh_transfer_file` for both PNGs | SKIPPED (single agent) |
| Mesh KNOCK + dispatch traces | SKIPPED (no mesh) |
| Telegram delivery | SKIPPED unless TELEGRAM_BOT_TOKEN set |

The final-artifact collection writes `OUT_DIR/final-artifact.md`
from `/sandbox/incoming/brief.md`.
