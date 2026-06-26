# kars Full-Stack Demo

One `kubectl apply` provisions everything kars can wire to a single agent:

| # | CRD | Purpose |
|---|---|---|
| 1 | `InferencePolicy` | Which LLM (gpt-4.1), 500K/day token cap, content safety on |
| 2 | `KarsMemory` | Foundry memory store binding, 30-day retention, auto-cleanup |
| 3 | `McpServer` | External MCP server registration |
| 4 | `ToolPolicy` | Rate-limit `web_search` to 60/min |
| 5 | `KarsSandbox` | The agent itself — references #1 by name, the rest by label |

## Apply

```bash
# Point at whichever cluster you want:
kubectl apply --context kars-aks -f examples/full-stack-demo/demo.yaml
```

## Watch

```bash
kubectl get karssandbox,inferencepolicy,karsmemory,mcpserver,toolpolicy \
  -n kars-system -l kars.azure.com/sandbox=demo-agent
```

Expected output after ~30s:

```
NAME                                            STATUS    AGE
karssandbox.kars.azure.com/demo-agent           Running   30s

NAME                                                       AGE
inferencepolicy.kars.azure.com/demo-agent-inference        30s

NAME                                              SANDBOX      STORE                SCOPE                PHASE       AGE
karsmemory.kars.azure.com/demo-agent-memory       demo-agent   memory-demo-agent    agent_demo-agent     Compiled    30s

NAME                                              URL                            PRODUCTION   PHASE      AGE
mcpserver.kars.azure.com/demo-agent-mcp           https://mcp.example.com/sse    false        Ready      30s

NAME                                              TOOL          DAILYCAP   PHASE      AGE
toolpolicy.kars.azure.com/demo-agent-tools        web_search               Ready      30s
```

## Open the agent's Web UI

```bash
kars connect demo-agent --context kars-aks
```

## See it in the operator dashboard

```bash
kars operator --context kars-aks
```

The agent shows up in the top table with `[i]` to drill into per-agent panels including the memory, MCP, and tool policy.

## Tear down

```bash
kubectl delete -f examples/full-stack-demo/demo.yaml
```

Because `KarsMemory.spec.deleteOnSandboxDelete: true`, the memory store contents are also wiped.

## Variations

- **No memory** — comment out the `KarsMemory` doc
- **No MCP** — comment out the `McpServer` doc
- **Stricter tool policy** — change `approval.mode: human` in `ToolPolicy` so every tool call needs human approval (configure `channel` to route to Slack/Teams)
- **Multiple tool policies** — duplicate the `ToolPolicy` doc with a different `tool:` selector for each tool
- **Different model** — change `modelPreference.primary.deployment` in `InferencePolicy`; the agent picks it up on the next router restart
