#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# ClawSandbox CR factory — produces small, valid CRs from runtime + name.
#
# Sourced by scenarios/*.sh. Every helper writes its YAML to stdout so
# callers can pipe to `kubectl apply -f -`.
#
# S13 phase2-config-authority-refs: ClawSandbox.spec now requires
# `runtime`, `sandbox`, and `inferenceRef`. Each helper therefore emits
# *two* documents — a sibling InferencePolicy and the ClawSandbox itself
# — separated by `---`. The test harness `kubectl apply -f -` consumes
# both in one round-trip.

# Common metadata block. Caller must set:
#   $1 = name
#   $2 = namespace
_meta() {
    cat <<EOF
apiVersion: azureclaw.azure.com/v1alpha1
kind: ClawSandbox
metadata:
  name: ${1}
  namespace: ${2}
  labels:
    azureclaw.azure.com/test-suite: manual-e2e
EOF
}

# Sibling InferencePolicy + the runtime-agnostic spec.sandbox /
# spec.inferenceRef tail. Inserted at the bottom of every CR helper so
# the manifest validates against the v1alpha1 schema.
#   $1 = sandbox name
#   $2 = namespace
_inference_policy() {
    cat <<EOF
---
apiVersion: azureclaw.azure.com/v1alpha1
kind: InferencePolicy
metadata:
  name: ${1}-inference
  namespace: ${2}
  labels:
    azureclaw.azure.com/sandbox: ${1}
    azureclaw.azure.com/test-suite: manual-e2e
spec:
  appliesTo:
    sandboxName: ${1}
  modelPreference:
    primary:
      provider: azure-openai
      deployment: gpt-4.1
  tokenBudget:
    dailyTokens: 100000
    perRequestTokens: 32000
EOF
}

# Common spec.sandbox + spec.inferenceRef tail. Indented under spec:
# (no leading dashes — caller has already opened the document).
_sandbox_tail() {
    cat <<EOF
  sandbox:
    isolation: enhanced
    runAsNonRoot: true
    readOnlyRootFilesystem: true
    allowPrivilegeEscalation: false
    writablePaths:
      - /sandbox
      - /tmp
  inferenceRef:
    name: ${1}-inference
EOF
}

cr_openclaw() {
    _meta "$1" "$2"
    cat <<EOF
spec:
  runtime:
    kind: OpenClaw
    openclaw: {}
EOF
    _sandbox_tail "$1"
    _inference_policy "$1" "$2"
}

cr_openai_agents() {
    _meta "$1" "$2"
    cat <<EOF
spec:
  runtime:
    kind: OpenAIAgents
    openaiAgents:
      pythonVersion: "3.12"
EOF
    _sandbox_tail "$1"
    _inference_policy "$1" "$2"
}

cr_anthropic() {
    _meta "$1" "$2"
    cat <<EOF
spec:
  runtime:
    kind: Anthropic
    anthropic:
      pythonVersion: "3.12"
EOF
    _sandbox_tail "$1"
    _inference_policy "$1" "$2"
}

cr_maf_python() {
    _meta "$1" "$2"
    cat <<EOF
spec:
  runtime:
    kind: MicrosoftAgentFramework
    microsoftAgentFramework:
      language: python
EOF
    _sandbox_tail "$1"
    _inference_policy "$1" "$2"
}

cr_langgraph_python() {
    _meta "$1" "$2"
    cat <<EOF
spec:
  runtime:
    kind: LangGraph
    langGraph:
      language: python
EOF
    _sandbox_tail "$1"
    _inference_policy "$1" "$2"
}

cr_langgraph_typescript() {
    _meta "$1" "$2"
    cat <<EOF
spec:
  runtime:
    kind: LangGraph
    langGraph:
      language: typescript
EOF
    _sandbox_tail "$1"
    _inference_policy "$1" "$2"
}

cr_pydantic_ai() {
    _meta "$1" "$2"
    cat <<EOF
spec:
  runtime:
    kind: PydanticAi
    pydanticAi:
      pythonVersion: "3.12"
EOF
    _sandbox_tail "$1"
    _inference_policy "$1" "$2"
}

# BYO with intentional config so byo-strict admission rejects it
# (used in scenarios/byo-strict.sh).
cr_byo_strict_invalid() {
    _meta "$1" "$2"
    cat <<EOF
spec:
  byoStrict: true
  runtime:
    kind: Byo
    byo:
      image: "mcr.microsoft.com/oss/v2/library/busybox:latest"
EOF
    _sandbox_tail "$1"
    _inference_policy "$1" "$2"
}

# Mesh-enabled OpenClaw with explicit allow for the named peer.
# $3 = peer name, $4 = peer mesh tier (e.g. "trusted")
cr_openclaw_mesh() {
    _meta "$1" "$2"
    cat <<EOF
spec:
  runtime:
    kind: OpenClaw
    openclaw: {}
  agt:
    mesh:
      enabled: true
      peers:
        - name: ${3}
          tier: ${4:-trusted}
EOF
    _sandbox_tail "$1"
    _inference_policy "$1" "$2"
}

cr_dispatch() {
    # Dispatch by runtime alias used by the CI harness too:
    #   openclaw, oai-agents, maf-python, anthropic,
    #   langgraph, langgraph-typescript, pydantic-ai
    local runtime="$1" name="$2" ns="$3"
    case "$runtime" in
        openclaw)             cr_openclaw "$name" "$ns" ;;
        oai-agents)           cr_openai_agents "$name" "$ns" ;;
        anthropic)            cr_anthropic "$name" "$ns" ;;
        maf-python)           cr_maf_python "$name" "$ns" ;;
        langgraph)            cr_langgraph_python "$name" "$ns" ;;
        langgraph-typescript) cr_langgraph_typescript "$name" "$ns" ;;
        pydantic-ai)          cr_pydantic_ai "$name" "$ns" ;;
        *)
            echo "ERROR: unknown runtime '${runtime}'" >&2
            return 1
            ;;
    esac
}

# Convenience: list every runtime alias the manual matrix exercises.
all_runtime_aliases() {
    cat <<EOF
openclaw
oai-agents
anthropic
maf-python
langgraph
langgraph-typescript
pydantic-ai
EOF
}
