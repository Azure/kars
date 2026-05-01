# AzureClaw Examples

End-to-end blueprints you can `kubectl apply -f` after running `azureclaw up`.

| Example | Runtime | What it shows |
|---------|---------|---------------|
| [`basic-agent`](basic-agent/) | OpenClaw | Minimal sandboxed agent with default security posture (seccomp, RO rootfs, egress guard, governance). |
| [`confidential-agent`](confidential-agent/) | OpenClaw | Same as `basic-agent` plus Confidential Containers (CVM workers, attested boot). |
| [`telegram-agent`](telegram-agent/) | OpenClaw | OpenClaw agent wired to a Telegram channel via the channel-plugin pattern. |
| [`demo-clawshield`](demo-clawshield/) | OpenClaw (×3) | Multi-tenant attack-simulation demo (poisoned document, two victim tenants, isolation proof). |
| [`openai-agents-quickstart`](openai-agents-quickstart/) | OpenAIAgents (Python) | Hosts an unmodified OpenAI Agents SDK app inside an AzureClaw sandbox. Same security as default. |
| [`maf-quickstart`](maf-quickstart/) | MicrosoftAgentFramework (Python) | Hosts an unmodified Microsoft Agent Framework app inside an AzureClaw sandbox. |

All examples share the same control-plane install and isolation guarantees
— only the agent runtime image changes. See
[`docs/api/crd-reference.md`](../docs/api/crd-reference.md) for the full
`ClawSandbox.spec.runtime` schema.

For broader patterns (developer inner-loop, enterprise self-hosted,
managed public offload, cross-org federation, sovereign / air-gapped),
see [`docs/blueprints/`](../docs/blueprints).
