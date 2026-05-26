# Examples catalogue

Eight end-to-end blueprints live under [`examples/`](https://github.com/Azure/azureclaw/tree/main/examples). Each one is a self-contained `kubectl apply -f` after `azureclaw up`. All examples share the same control-plane install and isolation guarantees — only the agent runtime image changes.

## Single-agent runtime quickstarts

| Example | Runtime | Verification status | What it shows |
|---|---|---|---|
| [`basic-agent`](https://github.com/Azure/azureclaw/tree/main/examples/basic-agent) | `OpenClaw` | YAMLs apply (dry-run + AKS); runtime path exercised by exec-brief harness | The smallest possible deployment — minimal sandbox with default isolation (seccomp-strict, RO rootfs, egress guard, AGT governance). **Start here.** |
| [`telegram-agent`](https://github.com/Azure/azureclaw/tree/main/examples/telegram-agent) | `OpenClaw` | YAMLs apply; requires real Telegram bot token to exercise channel | OpenClaw agent wired to a Telegram channel via the [channel-plugin pattern](channels-plugins.md). |
| [`confidential-agent`](https://github.com/Azure/azureclaw/tree/main/examples/confidential-agent) | `OpenClaw` + Kata VM isolation | ✅ Verified live on AKS with a `katapool` Kata-enabled nodepool (`runtimeClassName: kata-vm-isolation`, pod 2/2 Running). Won't schedule on plain `kind` clusters that lack the `kata-vm-isolation` RuntimeClass — that's expected. | The basic-agent shape upgraded to Kata VM isolation — dedicated kernel per pod, container-escape attacks trapped inside the VM. |
| [`openai-agents-quickstart`](https://github.com/Azure/azureclaw/tree/main/examples/openai-agents-quickstart) | `OpenAIAgents` (Python) | YAMLs apply once you swap `REPLACE-ME/...` with your image | Hosts an unmodified [OpenAI Agents SDK](https://github.com/openai/openai-agents-python) app. The adapter transparently routes `api.openai.com` through the local inference router. |
| [`maf-quickstart`](https://github.com/Azure/azureclaw/tree/main/examples/maf-quickstart) | `MicrosoftAgentFramework` (Python) | YAMLs apply once you swap `REPLACE-ME/...` with your image | Hosts an unmodified [Microsoft Agent Framework](https://github.com/microsoft/agent-framework) app. |
| [`byo-quickstart`](https://github.com/Azure/azureclaw/tree/main/examples/byo-quickstart) | `BYO` | Builds + applies cleanly; runtime requires you to bring an image | Brings any container image under the BYO contract. Includes a tiny FastAPI reference agent. |

## Multi-agent attack-simulation demos

| Example | Shape | Verification status | What it shows |
|---|---|---|---|
| [`demo-clawshield`](https://github.com/Azure/azureclaw/tree/main/examples/demo-clawshield) | Three OpenClaw agents in three namespaces (Contoso Bank, Fabrikam Legal, Northwind Trade) | ✅ All three sandboxes verified live: contoso + northwind 2/2 Running on standard nodes; fabrikam 2/2 Running on AKS Kata nodepool (`isolation: confidential`). The full attack-simulation script additionally requires three running sandboxes with real model deployments. | Multi-tenant isolation proof. A poisoned document attempts to exfiltrate across tenants; the NetworkPolicy + egress guard + governance layer each block it independently. |
| [`lethal-trifecta-demo`](https://github.com/Azure/azureclaw/tree/main/examples/lethal-trifecta-demo) | Two OpenClaw agents (vanilla vs. AzureClaw-managed) | `scripts/deploy.sh` materialises both deployments on local-k8s and AKS (pods reach `Ready`). `run-attack.sh` additionally requires (a) a working OpenClaw runtime config on the `naked-claw` side so the vanilla pod doesn't crashloop at startup, and (b) the operator-flow break-glass label `azureclaw.azure.com/break-glass=true` on `azureclaw-realestate-agent` to bypass the `ValidatingAdmissionPolicy/azureclaw-sandbox-exec-ban` ([context](use-cases/exec-brief-walkthrough.md#proof-the-agent-cannot-call-foundry-directly)) ValidatingAdmissionPolicy for the demo `kubectl exec` path. | Reproduces the [Claude Cowork file-exfiltration attack](https://www.promptarmor.com/resources/claude-cowork-exfiltrates-files) (Jan 2026). Six independent layers — each one alone catches the attack on the AzureClaw-managed side. **Recommended launch demo for the deploy-time defense story; the attack-simulation path requires the prerequisites above.** |

> **What "verification status" means here:** "YAMLs apply" = `kubectl apply --dry-run=client` succeeds against the published CRDs; "exercised by ..." = at least one of the maintainers has run the listed runbook end-to-end against a live cluster. Where the verification status mentions a credential or hardware prerequisite, those must be present on your side for the example to fully run.

## Prerequisites — common to all examples

Every example assumes:

1. `azureclaw up` (or `azureclaw dev` for the laptop path) has been run.
2. Your control plane has resolved the sandbox image (the controller sets `SANDBOX_IMAGE` to the image it built or pulled — see [Operations → Image versioning](operations/image-versioning.md)).
3. For Foundry-backed runs, the `InferencePolicy` references a model deployment that actually exists in your Foundry project.

The YAMLs intentionally do **not** pin `runtime.openclaw.image` or `runtime.openclaw.version` — the controller's `SANDBOX_IMAGE` default is the authoritative source. Pinning here would override that and likely break the example for anyone not running our internal registry.

## What you'll need to replace

The two SDK quickstarts (`openai-agents-quickstart`, `maf-quickstart`) reference a placeholder agent image:

```yaml
runtime:
  openaiAgents:                  # or microsoftAgentFramework
    agentCode:
      oci:
        image: REPLACE-ME/your-agent:latest
```

There is no published agent image for these — you supply your own. The README in each directory shows the exact swap. (The BYO quickstart has the same `REPLACE_ME` pattern for the same reason.)

## What none of these examples cover

- **Real cross-org A2A traffic** — that's the [A2A gateway](architecture/a2a-gateway.md) operations runbook, not a single-YAML demo.
- **Production-grade observability wiring** — the demos rely on `kubectl logs` and `kubectl describe`. For real ops, see [Operations → Observability](operations/README.md).
- **GitOps rollout** — see the [GitOps blueprint](operations/gitops.md) for Argo/Flux + signed-image rollout.

## See also

- [Runtimes](runtimes.md) — what each runtime kind does and which adapter image powers it
- [`examples/README.md`](https://github.com/Azure/azureclaw/blob/main/examples/README.md) — the same catalogue, in-repo
- [Use cases](use-cases.md) — patterns these examples are concrete instances of
