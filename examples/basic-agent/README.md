# Basic Agent — minimal AzureClaw example

The smallest possible end-to-end AzureClaw deployment: one OpenClaw
agent in a `ClawSandbox` with the default isolation posture.

Use this as the **template you copy-paste from** when standing up a new
agent. The other examples in this directory are variants of this same
shape.

## What it ships

A single `clawsandbox.yaml` containing two CRs:

| Resource | Purpose |
|---|---|
| `InferencePolicy/my-assistant-inference` | Provider, model, Content-Safety toggle, token budget |
| `ClawSandbox/my-assistant` | Runtime kind, isolation level, allowed egress endpoints |

The `ClawSandbox` references the `InferencePolicy` by name via
`spec.inferenceRef`. They live in the same namespace
(`azureclaw-system`) and the controller reconciles the pair into:

- A dedicated namespace for the sandbox (`azureclaw-my-assistant`)
- A 3-container Pod: `init: egress-guard` + `openclaw` + `inference-router`
- `seccomp: azureclaw-strict`, read-only rootfs, non-root, no
  privilege-escalation
- A `NetworkPolicy` allowing only the listed egress endpoints
- An audit pipeline + governance hooks

## Default posture

| Layer | Setting |
|---|---|
| Runtime | `OpenClaw 2026.3.13` |
| Isolation | `enhanced` (runc + strict seccomp + RO rootfs) |
| Model | `azure-openai/gpt-4.1` (via Foundry; switch to GitHub Models with `azureclaw dev`) |
| Content Safety | `requirePromptShields: true` |
| Token budget | 500k/day, 128k/request |
| Egress | github.com + api.github.com only |

## Deploy

```bash
# Prereq: an AzureClaw cluster (azureclaw up) with kubectl context set
kubectl apply -f examples/basic-agent/clawsandbox.yaml

# Watch the controller reconcile the sandbox
kubectl get clawsandbox my-assistant -n azureclaw-system -w
```

Once `STATUS=Ready`, connect:

```bash
azureclaw connect my-assistant
```

## Customize

- **Different model** — edit `spec.modelPreference.primary.deployment`
  in the `InferencePolicy`. The model must exist as a Foundry
  deployment (or be available on GitHub Models if you switched
  providers).
- **Different egress allowlist** — edit
  `spec.networkPolicy.allowedEndpoints` in the `ClawSandbox`. **Always
  scope by method** (e.g. `methods: ["GET"]`) — domain-only allowlists
  are bypassable by prompt injection, see
  [`examples/lethal-trifecta-demo`](../lethal-trifecta-demo/).
- **Tighter budget** — lower `spec.tokenBudget.perRequestTokens` in the
  `InferencePolicy`.

## Cleanup

```bash
kubectl delete -f examples/basic-agent/clawsandbox.yaml
```

## See also

- [`examples/confidential-agent`](../confidential-agent/) — same example
  on Kata VM isolation
- [`examples/lethal-trifecta-demo`](../lethal-trifecta-demo/) — the same
  default posture being attacked, layer by layer
- [`docs/api/crd-reference.md`](../../docs/api/crd-reference.md) — full
  CRD field reference
