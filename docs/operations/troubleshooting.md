# Troubleshooting

Start with Kubernetes status, then narrow to the controller, sandbox, router,
policy, and external dependency.

## First five commands

```bash
kubectl -n kars-system get pods
kubectl -n kars-system get karssandboxes,karstasks,karsteams
kubectl -n kars-system describe karssandbox <name>
kubectl -n kars-system logs deploy/kars-controller --since=15m
kubectl get events -A --sort-by=.lastTimestamp | tail -100
```

## Symptom guide

| Symptom | Check | Common cause |
|---|---|---|
| Sandbox stays Pending | Pod events and node capacity | Image pull, taint/toleration, quota, admission denial |
| Sandbox is Running but agent cannot answer | Router and runtime logs | Provider auth, model route, mesh delivery, exhausted budget |
| MCP tools are missing | `McpServer.status`, router startup logs | Not Ready, schema probe failed, sandbox not allowed |
| MCP worked before restart but now fails | Router `tools/list` probe logs | Stale upstream session; upgrade router if recovery is absent |
| Playwright resets to `about:blank` | MCP session logs | Session keepalive or non-isolated server configuration |
| Egress returns 403 | learned domains, approvals, router logs | Strict allowlist or expired approval |
| Mesh peer is undiscoverable | relay, registry, runtime logs | identity/prekey registration or trust threshold |
| Task reports success with an error string | mission output and runtime logs | outdated runtime/controller deliverable classification |
| `kubectl exec` is denied | namespace labels and admission policy | expected sandbox exec ban; use `kars connect` or audited break-glass |

## Managed MCP diagnostics

```bash
kubectl -n kars-system get mcpserver <name> -o yaml
kubectl -n kars-mcp get deploy,svc,networkpolicy
kubectl -n kars-<sandbox> logs deploy/<sandbox> -c inference-router --since=15m
```

`Ready=True` should include the observed generation, tool count, and schema
digest. A running MCP pod without a successful protocol probe is not ready.

## Mesh diagnostics

```bash
kubectl -n agentmesh get pods
kubectl -n agentmesh logs -l app=agentmesh-relay --since=15m
kubectl -n agentmesh logs -l app=agentmesh-registry --since=15m
```

Do not start a second Hermes `MeshClient` inside a live pod; doing so can contend
for identity/prekey ownership. Inspect the daemon logs and identity file only.

## Collecting an escalation bundle

Include:

- Kars and Kubernetes versions;
- the affected CR YAML with secrets removed;
- pod events;
- controller and router logs for the failure window;
- relevant condition reasons and trace IDs;
- CNI and node/runtime details;
- exact reproduction steps.

Never include provider tokens, GitHub App private keys, session cookies, AGT
identity seeds, or Kubernetes service-account tokens.
