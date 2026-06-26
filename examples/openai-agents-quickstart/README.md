# OpenAI Agents Python — Quickstart

This blueprint hosts an [OpenAI Agents SDK](https://github.com/openai/openai-agents-python) (Python)
agent inside a kars sandbox. The agent runs unmodified — kars
provides isolation, governance, and egress control without touching SDK code.

## What you get

- **Same security posture as the default OpenClaw runtime.** Seccomp profile,
  read-only rootfs, egress guard, AGT governance, AgentMesh E2E, and
  inference router are all identical.
- **Model selection through `InferencePolicy`** — not inlined
  in the sandbox spec. Adjust the policy CR to swap models without redeploying
  the agent.
- **No code changes in the SDK app.** The adapter image (`sandbox-images/openai-agents/`)
  sets `OPENAI_BASE_URL=http://127.0.0.1:8443/v1` and `HTTPS_PROXY=http://127.0.0.1:8444`
  so the agent's outbound HTTPS to `api.openai.com` is transparently routed through
  the inference router.

## Apply

```bash
kars up                                 # one-time control-plane install
kubectl apply -f examples/openai-agents-quickstart/karssandbox.yaml
kubectl get karssandboxes -n kars-system oai-quickstart
```

The controller patches the sandbox pod to launch the OpenAI Agents adapter
instead of OpenClaw, then waits for the InferencePolicy to be ready before
flipping `Ready=True`.

## Bring your own agent code

Two flavours of `agentCode:` are supported:

```yaml
# Production: agent code baked into a signed OCI image
runtime:
  kind: OpenAIAgents
  openaiAgents:
    agentCode:
      oci:
        image: ghcr.io/your-org/your-agent:v1.2.3

# Dev iteration: clone from git at pod start
runtime:
  kind: OpenAIAgents
  openaiAgents:
    agentCode:
      git:
        url: https://github.com/your-org/your-agent
        ref: main
        path: agents/my-agent
```

Your image must contain `pyproject.toml` (or `requirements.txt`) and an
entrypoint Python script. By default, the adapter runs `python -u agent.py`;
override via `runtime.openaiAgents.entrypoint`.

## What's next

- Combine with **`A2AAgent`** to publish your agent over the JWS-authenticated
  A2A gateway.
- Add a **`ToolPolicy`** CR and reference it from `governance.toolPolicyRef`
  to enforce rate-limits, redaction, and per-tool RBAC.
- See [`docs/blueprints/`](../../docs/blueprints) for end-to-end patterns
  (developer inner-loop, enterprise self-hosted, cross-org federation).
