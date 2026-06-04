# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# scenarios/mesh-roundtrip-hermes/config.sh — Hermes Act 2 mesh
# end-to-end smoke test.
#
# Drives `hermes -z` on mesh-ping-hermes with a prompt that uses
# the real kars_mesh_send + kars_mesh_await tools to talk to a
# sibling sandbox (mesh-pong-hermes) running a Python echo daemon.
#
# This scenario does NOT use the OpenClaw gateway HTTP path
# (port 18789) — Hermes runs as a daemon without an HTTP API. The
# harness's Hermes-aware platform_post_prompt() uses `kubectl exec
# -c agent -- hermes -z` instead. The exec-ban VAP only targets
# container name 'openclaw'; Hermes' container is 'agent', so this
# is policy-compliant.

# Parent (the LLM-driven sandbox).
SCENARIO_SANDBOX="mesh-ping-hermes"
SCENARIO_SUB_SANDBOXES=("mesh-pong-hermes")
SCENARIO_PROMPT_FILE="prompt.txt"

# Mesh handshake + LLM round-trip should fit in 5 min comfortably.
SCENARIO_WATCHDOG_SECS=420

# Hermes runtime → driver uses `hermes -z` over kubectl exec, not
# port-forward + /v1/chat/completions. The aks.sh helper switches
# on this hint.
SCENARIO_RUNTIME="hermes"
SCENARIO_PROMPT_DRIVER="hermes-exec"

# Daemon-on-sub-sandbox: before posting the parent prompt, the
# driver copies daemon.py into the sub-sandbox and starts it in
# the background, then waits for the ECHO_READY marker on its
# stdout before continuing.
SCENARIO_DAEMON_SUB="mesh-pong-hermes"
SCENARIO_DAEMON_SCRIPT="daemon.py"
SCENARIO_DAEMON_READY_MARKER="ECHO_READY"

# verify.py expects this single line in transcript.log on success.
SCENARIO_EXPECT_LINE="RECEIVED:echo(mesh-pong-hermes): hello-from-ping"

# No FS artifacts to collect — this scenario produces only chat output.
