# AzureClaw Azure Monitor Dashboards

## Token Usage per Sandbox (KQL)
```kql
InsightsMetrics
| where Name == "azureclaw_tokens_total"
| extend sandbox = tostring(parse_json(Tags).sandbox),
         model = tostring(parse_json(Tags).model),
         direction = tostring(parse_json(Tags).direction)
| summarize TotalTokens = sum(Val) by sandbox, model, direction, bin(TimeGenerated, 1h)
| order by TimeGenerated desc
```

## Inference Latency (KQL)
```kql
InsightsMetrics
| where Name == "azureclaw_inference_latency_seconds"
| extend sandbox = tostring(parse_json(Tags).sandbox),
         model = tostring(parse_json(Tags).model)
| summarize AvgLatency = avg(Val), P95 = percentile(Val, 95), P99 = percentile(Val, 99) by sandbox, model, bin(TimeGenerated, 5m)
| order by TimeGenerated desc
```

## Request Counts by Status (KQL)
```kql
InsightsMetrics
| where Name == "azureclaw_inference_requests_total"
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
| where Name == "azureclaw_tokens_total"
| extend sandbox = tostring(parse_json(Tags).sandbox)
| summarize DailyTokens = sum(Val) by sandbox, bin(TimeGenerated, 1d)
| where DailyTokens > 80000  // 80% of 100k default budget
```

## Cost Estimation
```kql
InsightsMetrics
| where Name == "azureclaw_tokens_total"
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
