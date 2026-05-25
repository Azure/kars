# exec-brief end-to-end harness

End-to-end test that provisions the full five-CRD AzureClaw surface
(`ClawSandbox` + `InferencePolicy` + `ToolPolicy` + `ClawMemory` + `McpServer`)
on the AKS cluster you are currently `kubectl`-logged into, drives a complex
multi-sub-agent executive-brief prompt through it, monitors every layer live,
and verifies the 7 acceptance checks the prompt declares.

> This harness assumes the cluster is already up and the Helm chart is
> installed (`azureclaw up`). It does not provision Azure resources.

## Layout

```
scenarios/   six CRD manifests applied in order (00-namespace → 05-clawsandbox)
prompts/     the verbatim executive-brief prompt (used as stdin to the agent)
drive.sh     applies CRDs, creates the telegram secret, waits for Ready,
             then pipes the prompt into `azureclaw connect`
monitor.sh   colour-coded live tail of K8s events / controller / router /
             relay / registry / pod, also written as JSONL to trace.jsonl
verify.py    7 acceptance checks against trace.jsonl + transcript.log
run.sh       runs monitor + drive + verify, writes everything to out/<runId>/
```

## Prerequisites

- `kubectl` pointing at an AKS cluster where `azureclaw up` has succeeded
  (all five CRDs installed, controller + relay + registry running).
- `azureclaw` CLI on `$PATH`.
- `python3` (3.10+) for the verifier — uses only stdlib.
- Optional: `TELEGRAM_BOT_TOKEN` to satisfy the telegram acceptance check.
  If unset, that check is skipped (not failed).
- The Foundry project referenced by the existing `InferencePolicy` cluster-default
  must have the **AI Foundry Web Search** and **gpt-image-1** model deployments
  enabled — the prompt exercises both.

## Run

```bash
cd tools/exec-brief-e2e
TELEGRAM_BOT_TOKEN=xxxxx ./run.sh
```

Watch the live colour-coded timeline in the terminal. When the driver returns,
the verifier runs automatically and prints the seven check results.

## Artifacts

Each run lands in `out/<UTC-timestamp>/` (with `out/latest` symlinked):

| file              | what                                                    |
|-------------------|---------------------------------------------------------|
| `apply.log`       | `kubectl apply` output for every scenario file          |
| `drive.log`       | the driver's own stdout/stderr                          |
| `monitor.log`     | full coloured monitor output (mirrors stdout)           |
| `trace.jsonl`     | one line per event from each watched source             |
| `transcript.log`  | what the agent posted back to `azureclaw connect`       |
| `verify.json`     | machine-readable acceptance-check results               |

## What the 9 acceptance checks look at

1. **≥6 distinct 2026 sources cited** — unique non-infra URLs in the transcript.
2. **4×4 metrics scorecard** — `"metrics"` JSON block + the four axis labels.
3. **Hero image via gpt-image-1** — `/images/generations` calls in the router
   log + 1024×1024 dimension mention in the transcript.
4. **Chart via Foundry code-exec** — `/code/sessions` calls in the router log.
5. **≥3 distinct sibling pairs on relay** — analyst↔viz, analyst↔writer,
   viz↔writer in the relay log.
6. **≥5 Telegram status posts** — `sendMessage` calls to `api.telegram.org`
   (skipped if no token).
7. **Brief shape** — ~900 words, hero + chart references present.
8. **Egress clean under Strict mode** — zero NetworkPolicy denials and zero
   controller `BlockedBuffer` entries. Proves the inline allowlist on
   `ClawSandbox.spec.networkPolicy` matched real traffic exactly.
9. **MCP traffic observed** — the analyst hit the DeepWiki MCP for ≥1
   platform deep-dive; the router proxied `/mcp/` calls and the brief
   cites at least one deepwiki reference.

## Why there is no separate `NetworkEgress` CRD

Egress allowlists live **inline** on `ClawSandbox.spec.networkPolicy`,
not in a sibling CR. The controller renders a real K8s `NetworkPolicy`
from the closure of `allowedEndpoints` when `egressMode: Strict`. The
production pattern replaces the inline list with `allowlistRef`
pointing at a cosign-signed OCI artifact (see `azureclaw egress sign`)
so the policy is content-addressed and tamper-evident, but it is still
the same field on the same parent CR. (There is an `EgressApproval`
CRD, but that one carries runtime *approval requests* — not policy.)

Exit code 0 ⇒ all checks passed, 1 ⇒ at least one failed.

## Troubleshooting

| symptom                               | likely cause                                         |
|---------------------------------------|------------------------------------------------------|
| `ERR CRD ... missing`                 | run `azureclaw up` first                             |
| sandbox `Ready=False` after 10 min    | image pull / RBAC — check controller logs            |
| router log empty after Ready          | `azureclaw connect` couldn't reach 18789 — port busy |
| 0 sibling pairs on relay              | sub-agents never spawned (check pod logs for AGT)    |
| 0 telegram posts                      | bot token wrong, or chat not started with the bot    |
| acceptance #3 fails                   | gpt-image-1 deployment missing in your Foundry proj  |

## Scope

This harness is intentionally **read-only after provisioning** — it never
mutates the prompt to coax success and never edits the brief returned by
the agent. The point is to detect regressions in the full stack
(CRDs, controller, router, mesh, governance, telegram, MCP, Foundry)
end-to-end with one command.
