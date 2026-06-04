# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# scenarios/exec-brief-hermes-single/config.sh — single-agent variant
# of exec-brief on the Hermes runtime.
#
# Differences vs the canonical exec-brief scenario:
#   - One sandbox (`execbrief-hermes`) doing the whole pipeline; no
#     analyst/viz/writer sub-agents → no kars_mesh_* needed.
#   - Runtime = Hermes (Nous Research) rather than OpenClaw. Same
#     plugin contract (kars_spawn family, foundry_*, http_fetch,
#     pre_tool_call governance hook).
#   - Runs Act 1 of the Hermes work (mesh stubs return clear
#     Act-2-not-ready errors), so the prompt explicitly tells the
#     agent to do the analyst/viz/writer steps itself instead of
#     spawning sub-agents.
#
# Used to validate the Hermes runtime adapter end-to-end on
# local-k8s and AKS without needing the Python AGT MeshClient that
# ships in Act 2.

SCENARIO_SANDBOX="execbrief-hermes"
SCENARIO_SUB_SANDBOXES=()
SCENARIO_PROMPT_FILE="prompt.txt"
SCENARIO_WATCHDOG_SECS=1500

# No sub-agents to harvest gateway logs from; the parent does the
# whole pipeline. Patterns for the parent log are inlined in
# verify.py logic (the driver only forwards SCENARIO_GREP_PATTERNS_*
# for sub-agent inspection).

# The final delivered artifact is the brief written by the single
# Hermes agent to its sandbox FS. The harness collects this into
# OUT_DIR/final-artifact.md so verify.py scores against the actual
# written file rather than any echoed text in the channel reply
# (which Foundry content_safety can finish_reason=content_filter
# mid-stream on long echoes).
SCENARIO_INCOMING_SANDBOX="execbrief-hermes"
SCENARIO_INCOMING_PATH="/sandbox/incoming/"
SCENARIO_FINAL_ARTIFACT_PATH="/sandbox/incoming/brief.md"
