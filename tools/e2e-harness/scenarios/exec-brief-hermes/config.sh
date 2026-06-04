# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# scenarios/exec-brief-hermes/config.sh — multi-agent Hermes mesh
# end-to-end smoke. Parent uses kars_spawn to launch 3 Hermes children
# (analyst, viz, writer), then mesh_send-coordinates them through the
# real Python AGT MeshClient. Differences vs the OpenClaw exec-brief
# scenario:
#   - Runtime kind: Hermes (the router's spawn endpoint reads
#     KARS_RUNTIME_KIND from the parent's env and stamps every child
#     with runtime.kind=Hermes — see inference-router/src/spawn/mod.rs).
#   - Driver: `hermes -z` over kubectl exec (no port-forward gateway).
#   - No telegram channel (`kars.azure.com/channels: none`).

SCENARIO_SANDBOX="execbrief-hermes-multi"
SCENARIO_SUB_SANDBOXES=("analyst" "viz" "writer")
SCENARIO_PROMPT_FILE="prompt.txt"
# Multi-agent mesh + LLM coordination is slow on a single-node kind
# cluster. Generous watchdog: each sub-agent does real Foundry calls.
SCENARIO_WATCHDOG_SECS=2400

SCENARIO_RUNTIME="hermes"
SCENARIO_PROMPT_DRIVER="hermes-exec"

# Sub-agent grep patterns harvested from each child's gateway log so
# verify.py can assert mesh tools fired without re-running the LLM.
SCENARIO_GREP_PATTERNS_analyst=("kars_mesh_send|foundry_web_search|file_write")
SCENARIO_GREP_PATTERNS_viz=("kars_mesh_await|foundry_code_execute|kars_mesh_send")
SCENARIO_GREP_PATTERNS_writer=("kars_mesh_await|file_write|kars_mesh_send")
