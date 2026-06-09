# Hermes Quickstart — minimal kars Hermes-runtime example

The smallest possible Hermes deployment: one [Hermes Agent](https://github.com/NousResearch/hermes-agent) (Nous Research, MIT) in a `KarsSandbox` with the default isolation posture, the kars plugin auto-loaded, AGT governance on, and the agent joined to the mesh.

Use this as the **template you copy-paste from** when standing up a new Hermes agent. For the OpenClaw counterpart, see [`examples/basic-agent/`](../basic-agent/).

## What it ships

A single `karssandbox.yaml` containing two CRs:

| Resource | Purpose |
|---|---|
| `InferencePolicy/hermes-assistant-inference` | Provider, model, Content-Safety toggle, token budget |
| `KarsSandbox/hermes-assistant` | Runtime kind (`Hermes`), isolation level, allowed egress endpoints |

The `KarsSandbox` references the `InferencePolicy` by name via `spec.inferenceRef`. They live in the same namespace (CRD enforces); cross-namespace refs are not allowed.

## Prerequisites

- A working kars cluster — `kars up` succeeded and `kubectl get pods -n kars-system` shows the controller, inference-router, and (if you accepted defaults) the AGT mesh relay/registry running.
- ACR loaded with `kars-runtime-hermes:latest` (default — `kars up` includes it; `kars push --only runtime-hermes --apply` if you skipped runtime images).
- Optional but recommended: a `KarsAuthConfig` in `kars-system` with `spec.foundryRbac` set so the controller auto-grants `Azure AI User` to every per-sandbox Entra Agent App. Without it, the agent still boots but stays at `tier=anonymous` on the operator panel.

## Apply

```bash
kubectl apply -f examples/hermes-quickstart/karssandbox.yaml
```

The controller takes ~30 seconds to provision the namespace, secrets, NetworkPolicy, inference-router sidecar, and the Hermes pod itself. Watch the agent come up:

```bash
kubectl get pods -n kars-hermes-assistant -w
```

You should see the pod transition `Init:0/1 → Init:1/1 → Running 2/2`.

## Talk to it

```bash
# From your laptop — port-forward the inference router's chat-completions surface.
kubectl port-forward -n kars-hermes-assistant deploy/hermes-assistant 8443:8443 &

# Send a prompt — note the router is the entry point, NOT Hermes' gateway.
TOKEN=$(kubectl get secret -n kars-hermes-assistant gateway-token -o jsonpath='{.data.token}' | base64 -d)
curl -sS -X POST -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"model":"hermes","messages":[{"role":"user","content":"Say hello"}],"max_tokens":50}' \
  http://127.0.0.1:8443/v1/chat/completions | jq -r '.choices[0].message.content'
```

## Drop into the agent's TUI (alternative)

Hermes ships a built-in terminal UI. `kars connect` recognises Hermes-runtime sandboxes and drops you into an interactive `hermes` REPL via `kubectl exec -it`:

```bash
kars connect hermes-assistant
```

(See [`docs/operator-tui.md`](../../docs/operator-tui.md) for the operator panel + cross-pod surface.)

## Tear it down

```bash
kubectl delete -f examples/hermes-quickstart/karssandbox.yaml
```

The controller reaps the namespace; it can take ~30 seconds for the validating-admission policies to release any cron-refresh pods that may still be terminating.

---

## What to try next

| Next step | Where to look |
|---|---|
| Run your own Hermes agent code instead of the smoke-test default | Add `spec.runtime.hermes.agentCode.oci.image` or `.git.url` — see [`docs/api/crd-reference.md#hermesconfig`](../../docs/api/crd-reference.md#hermesconfig). |
| Front the agent with a Telegram bot | `kars credentials update hermes-assistant --telegram-token <bot-token>` — full guide in [`docs/channels-plugins.md`](../../docs/channels-plugins.md). |
| Mesh between Hermes and OpenClaw | Apply [`examples/basic-agent/clawsandbox.yaml`](../basic-agent/clawsandbox.yaml) too — both will discover each other via the AGT registry and `kars_mesh_send` works in either direction. |
| Tighten the egress allowlist | Add or replace `spec.networkPolicy.allowedEndpoints[]` — see [`docs/egress-proxy.md`](../../docs/egress-proxy.md). |

## See also

- **[Hermes plugin reference](../../docs/hermes-plugin.md)** — tools, hooks, channels, plugins, mesh integration.
- **[CRD reference — `HermesConfig`](../../docs/api/crd-reference.md#hermesconfig)** — full schema for `spec.runtime.hermes.*`.
- **[Hermes troubleshooting runbook](../../docs/runbooks/hermes-troubleshooting.md)** — the five most common failure modes with concrete kubectl recipes.
