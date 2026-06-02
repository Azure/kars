# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# scenarios/exec-brief/config.sh — exec-brief-specific knobs the
# generic driver picks up via `source $SCENARIO_DIR/config.sh`.
#
# Required variables:
#   SCENARIO_SANDBOX     — name of the parent KarsSandbox the harness
#                          posts the prompt to (also the sandbox-NS
#                          stem: pod runs in kars-${SCENARIO_SANDBOX}).
#   SCENARIO_SUB_SANDBOXES
#                        — bash array of sub-agent sandbox names whose
#                          namespaces the driver should harvest
#                          gateway logs from in collect_artifacts.
#                          Empty array if the scenario has no
#                          sub-agents.
#   SCENARIO_PROMPT_FILE — relative path inside the scenario dir to the
#                          prompt text. Default: prompt.txt.
#   SCENARIO_GREP_PATTERNS_<subname>
#                        — bash-array per sub-agent of grep -E patterns
#                          the collect step applies to the sub-agent's
#                          /tmp/gateway.log inside the container.
#
# Optional:
#   SCENARIO_WATCHDOG_SECS — override per-scenario watchdog (default 1500s).

SCENARIO_SANDBOX="execbrief"
SCENARIO_SUB_SANDBOXES=("analyst" "viz" "writer")
SCENARIO_PROMPT_FILE="prompt.txt"
SCENARIO_WATCHDOG_SECS=2400

# Patterns harvested from each sub-agent's in-pod gateway log. The
# break-glass label is briefly applied (then removed) so the driver can
# exec into the container for these reads.
SCENARIO_GREP_PATTERNS_writer=("file_transfer_ack|mesh_transfer_file")
SCENARIO_GREP_PATTERNS_viz=("mesh_transfer_file|foundry_image_generation|downloaded_files")
SCENARIO_GREP_PATTERNS_analyst=("file_transfer_ack|mesh_transfer_file|foundry_web_search")

# Also dump the writer's incoming/ directory — definitive evidence the
# scorecard.png and hero.png arrived. Set to "" to skip.
SCENARIO_INCOMING_SANDBOX="writer"
SCENARIO_INCOMING_PATH="/sandbox/.openclaw/workspace/incoming/"

# Final delivered artifact: the parent agent's mesh-received brief.md.
# The harness collects this into OUT_DIR/final-artifact.md so verify.py
# can score against what was actually delivered instead of the parent's
# echoed reply (which Foundry content_safety can finish_reason=content_filter
# mid-stream when the agent verbatim-echoes a long brief). Without this,
# every successful run risks a 4/9-down-to-9/9 swing on the same artifact
# depending on whether the model's echo happened to trip a filter category.
SCENARIO_FINAL_ARTIFACT_SANDBOX="execbrief"
SCENARIO_FINAL_ARTIFACT_PATH="/sandbox/.openclaw/workspace/incoming/brief.md"
