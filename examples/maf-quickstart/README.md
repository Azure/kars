# Microsoft Agent Framework (MAF) — Quickstart

This blueprint hosts a [Microsoft Agent Framework](https://github.com/microsoft/agent-framework)
(Python) agent inside an Kars sandbox. The agent runs unmodified —
Kars provides isolation, governance, and egress control without
touching MAF code.

## What you get

- **Same security posture as the default OpenClaw runtime.** Seccomp profile,
  read-only rootfs, egress guard, AGT governance, AgentMesh E2E, and
  inference router are all identical.
- **Model selection through `InferencePolicy`** — adjust
  the policy CR to swap models without redeploying the agent (model choice is
  referenced from the policy, not inlined in the sandbox spec).
- **No code changes in the MAF app.** The adapter image
  (`sandbox-images/maf-python/`) sets `AZURE_OPENAI_ENDPOINT` /
  `AZURE_OPENAI_API_KEY` (synthetic, valid only inside the sandbox) so
  MAF's Azure OpenAI client routes through the local inference router.

## Apply

```bash
kars up                                 # one-time control-plane install
kubectl apply -f examples/maf-quickstart/karssandbox.yaml
kubectl get karssandboxes -n kars-system maf-quickstart
```

## Bring your own agent code

```yaml
# Production: signed OCI image
runtime:
  kind: MicrosoftAgentFramework
  microsoftAgentFramework:
    language: python
    agentCode:
      oci:
        image: ghcr.io/your-org/your-maf-agent:v1.2.3

# Dev iteration: clone from git at pod start
runtime:
  kind: MicrosoftAgentFramework
  microsoftAgentFramework:
    language: python
    agentCode:
      git:
        url: https://github.com/your-org/your-maf-agent
        ref: main
        path: agents/my-agent
```

Your image must contain `pyproject.toml` (or `requirements.txt`) and an
entrypoint Python script. Default entrypoint is `python -u agent.py`.

## Language matrix

| Language | Status | Notes |
|----------|---------|-------|
| `python` | ✅ shipped | `sandbox-images/maf-python/` adapter |
| `dotnet` | ⏳ planned | CRD accepts the value; controller stamps `RuntimeReady=False / AdapterMissing` until the .NET adapter image ships |

## What's next

- Combine with **`A2AAgent`** to publish your agent over the JWS-authenticated
  A2A gateway.
- Add a **`ToolPolicy`** CR to enforce rate-limits, redaction, and per-tool RBAC.
- See [`docs/blueprints/`](../../docs/blueprints) for end-to-end patterns.
