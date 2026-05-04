#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# ClawSandbox CR factory — produces small, valid CRs from runtime + name.
#
# Sourced by scenarios/*.sh. Every helper writes its YAML to stdout so
# callers can pipe to `kubectl apply -f -`.

# Common metadata block. Caller must set:
#   $1 = name
#   $2 = namespace
_meta() {
    cat <<EOF
apiVersion: azureclaw.io/v1alpha1
kind: ClawSandbox
metadata:
  name: ${1}
  namespace: ${2}
  labels:
    azureclaw.io/test-suite: manual-e2e
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
}

cr_maf_python() {
    _meta "$1" "$2"
    cat <<EOF
spec:
  runtime:
    kind: MicrosoftAgentFramework
    microsoftAgentFramework:
      language: Python
EOF
}

cr_langgraph_python() {
    _meta "$1" "$2"
    cat <<EOF
spec:
  runtime:
    kind: LangGraph
    langGraph:
      language: Python
EOF
}

cr_langgraph_typescript() {
    _meta "$1" "$2"
    cat <<EOF
spec:
  runtime:
    kind: LangGraph
    langGraph:
      language: TypeScript
EOF
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
