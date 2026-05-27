# Basic Agent — minimal kars example

The smallest possible end-to-end kars deployment: one OpenClaw
agent in a `KarsSandbox` with the default isolation posture.

Use this as the **template you copy-paste from** when standing up a new
agent. The other examples in this directory are variants of this same
shape.

## What it ships

A single `karssandbox.yaml` containing two CRs:

| Resource | Purpose |
|---|---|
| `InferencePolicy/my-assistant-inference` | Provider, model, Content-Safety toggle, token budget |
| `KarsSandbox/my-assistant` | Runtime kind, isolation level, allowed egress endpoints |

The `KarsSandbox` references the `InferencePolicy` by name via
`spec.inferenceRef`. They live in the same namespace
(`kars-system`) and the controller reconciles the pair into:

- A dedicated namespace for the sandbox (`kars-my-assistant`)
- A 3-container Pod: `init: egress-guard` + `openclaw` + `inference-router`
- `seccomp: kars-strict`, read-only rootfs, non-root, no
  privilege-escalation
- A `NetworkPolicy` allowing only the listed egress endpoints
- An audit pipeline + governance hooks

## Default posture

| Layer | Setting |
|---|---|
| Runtime | `OpenClaw` (image resolved by the controller from its `SANDBOX_IMAGE` env, set by `kars up`) |
| Isolation | `enhanced` (runc + strict seccomp + RO rootfs) |
| Model | `azure-openai/gpt-4.1` (via Foundry; switch to GitHub Models with `kars dev`) |
| Content Safety | `requirePromptShields: true` |
| Token budget | 500k/day, 128k/request |
| Egress | github.com + api.github.com only |

## Deploy

```bash
# Prereq: an kars cluster (kars up) with kubectl context set
kubectl apply -f examples/basic-agent/karssandbox.yaml

# Watch the controller reconcile the sandbox
kubectl get karssandbox my-assistant -n kars-system -w
```

Once `STATUS=Ready`, connect:

```bash
kars connect my-assistant
```

## Customize

- **Different model** — edit `spec.modelPreference.primary.deployment`
  in the `InferencePolicy`. The model must exist as a Foundry
  deployment (or be available on GitHub Models if you switched
  providers).
- **Different egress allowlist** — edit
  `spec.networkPolicy.allowedEndpoints` in the `KarsSandbox`. **Always
  scope by method** (e.g. `methods: ["GET"]`) — domain-only allowlists
  are bypassable by prompt injection, see
  [`examples/lethal-trifecta-demo`](../lethal-trifecta-demo/).
- **Tighter budget** — lower `spec.tokenBudget.perRequestTokens` in the
  `InferencePolicy`.

## Cleanup

```bash
kubectl delete -f examples/basic-agent/karssandbox.yaml
```

## See also

- [`examples/confidential-agent`](../confidential-agent/) — same example
  on Kata VM isolation
- [`examples/lethal-trifecta-demo`](../lethal-trifecta-demo/) — the same
  default posture being attacked, layer by layer
- [`docs/api/crd-reference.md`](../../docs/api/crd-reference.md) — full
  CRD field reference
