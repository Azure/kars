# kars Azure Monitor Dashboards

## Token Usage per Sandbox (KQL)
```kql
InsightsMetrics
| where Name == "kars_tokens_total"
| extend sandbox = tostring(parse_json(Tags).sandbox),
         model = tostring(parse_json(Tags).model),
         direction = tostring(parse_json(Tags).direction)
| summarize TotalTokens = sum(Val) by sandbox, model, direction, bin(TimeGenerated, 1h)
| order by TimeGenerated desc
```

## Inference Latency (KQL)
```kql
InsightsMetrics
| where Name == "kars_inference_latency_seconds"
| extend sandbox = tostring(parse_json(Tags).sandbox),
         model = tostring(parse_json(Tags).model)
| summarize AvgLatency = avg(Val), P95 = percentile(Val, 95), P99 = percentile(Val, 99) by sandbox, model, bin(TimeGenerated, 5m)
| order by TimeGenerated desc
```

## Request Counts by Status (KQL)
```kql
InsightsMetrics
| where Name == "kars_inference_requests_total"
| extend sandbox = tostring(parse_json(Tags).sandbox),
         model = tostring(parse_json(Tags).model),
         status = tostring(parse_json(Tags).status)
| summarize Requests = sum(Val) by sandbox, model, status, bin(TimeGenerated, 1h)
| order by TimeGenerated desc
```

## Token Budget Alerts
```kql
// Alert when any sandbox exceeds 80% of daily budget
InsightsMetrics
| where Name == "kars_tokens_total"
| extend sandbox = tostring(parse_json(Tags).sandbox)
| summarize DailyTokens = sum(Val) by sandbox, bin(TimeGenerated, 1d)
| where DailyTokens > 80000  // 80% of 100k default budget
```

## Cost Estimation
```kql
InsightsMetrics
| where Name == "kars_tokens_total"
| extend sandbox = tostring(parse_json(Tags).sandbox),
         model = tostring(parse_json(Tags).model),
         direction = tostring(parse_json(Tags).direction)
| summarize Tokens = sum(Val) by sandbox, model, direction, bin(TimeGenerated, 1d)
| extend CostUSD = case(
    model startswith "gpt-4.1" and direction == "input", Tokens * 0.002 / 1000,
    model startswith "gpt-4.1" and direction == "output", Tokens * 0.008 / 1000,
    model startswith "Phi-4", Tokens * 0.0001 / 1000,
    Tokens * 0.001 / 1000)
| summarize TotalCost = sum(CostUSD) by sandbox, model, bin(TimeGenerated, 1d)
```

## Mesh message counters

The router exports two `IntCounter` metrics, scraped by the
`kars-sandbox-router` PodMonitor:

- `kars_mesh_messages_sent_total`
- `kars_mesh_messages_received_total`

The `sandbox=<name>` label is injected at scrape time (relabel from
`__meta_kubernetes_pod_label_kars_azure_com_sandbox`). Each
counter ticks **once per WebSocket frame proxied through the relay**
(KNOCK, X3DH bundle, encrypted `mesh_send`, and the explicit
30 s `sendHeartbeat()` tick). WebSocket Ping/Pong keepalives and
registry HTTP calls (`/v1/agents/...`) are **not** counted.

Counters live in the router process and reset on pod restart.
A fresh sandbox typically shows `sent ≫ received` because it emits
≥ 1 KNOCK per known peer + a 30 s heartbeat tick, while inbound is
just the relay's KNOCK-ack until a real conversation starts.

```promql
# Per-sandbox sent rate (PromQL)
sum by (sandbox) (rate(kars_mesh_messages_sent_total[5m]))

# Per-sandbox received rate
sum by (sandbox) (rate(kars_mesh_messages_received_total[5m]))

# Fleet totals over 24 h
sum(increase(kars_mesh_messages_sent_total[24h]))
sum(increase(kars_mesh_messages_received_total[24h]))
```

For per-peer message attribution (sender→receiver pairs) the Rust
router currently only exposes this via the in-process atomic
counters reported on `/agt/status` (`trust_states[].interactions`)
— there is no Prometheus per-pair metric yet. The Headlamp Mesh
Topology and operator CLI both fall back to:

1. **Children count** from the `kars.azure.com/parent=<name>`
   label on sub-agent CRs (deterministic, derived from the K8s API).
2. **Trust-graph size** = `kars_agt_known_agents` (populates
   only after live traffic; resets on pod restart).
