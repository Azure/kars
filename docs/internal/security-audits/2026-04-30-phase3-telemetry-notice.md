# Security Audit — Phase 3 · README Data Collection / Telemetry Notice (FILE-TELEMETRY)

**Date:** 2026-04-30
**OSPO finding:** FILE-TELEMETRY (`docs/internal/2026-04-28-Azure-azureclaw.md`)
**Author:** Copilot <223556219+Copilot@users.noreply.github.com>
**Independent reviewer:** Pal Lakatos-Toth <pallakatos@microsoft.com>
**Capability scope:** Documentation change only — adds the canonical Microsoft OSPO Data
Collection block plus AzureClaw-specific telemetry inventory to `README.md`.
No production code paths changed.

---

## 1. Summary

OSPO audit finding FILE-TELEMETRY: the README references OpenTelemetry GenAI semantic
conventions but carries no Data Collection / opt-out notice as required by
`docs/releasing/general/data-collection.md`. This PR appends:

1. The canonical Microsoft OSS Data Collection block verbatim.
2. An AzureClaw-specific section documenting: what is collected, what is NOT
   collected (no prompt/completion text), where telemetry goes (your cluster only,
   not Microsoft by default), and opt-out instructions.

All claims in section 2 are grounded in primary source files listed below.

---

## 2. Telemetry inventory (source-grounded)

### 2.1 Prometheus metrics — `inference-router/src/metrics.rs`

All metric definitions live in `inference-router/src/metrics.rs`. The file uses
`prometheus::{register_int_counter_vec!, register_histogram_vec!, …}` macros. No
`opentelemetry::*` crate functions are called from this file or from any site that
imports it.

**Inference / token metrics:**

| Metric | Rust symbol | Labels |
|---|---|---|
| `azureclaw_inference_requests_total` | `INFERENCE_REQUESTS` | `sandbox`, `model`, `status` |
| `azureclaw_inference_latency_seconds` | `INFERENCE_LATENCY` | `sandbox`, `model` |
| `azureclaw_tokens_total` | `TOKENS_USED` | `sandbox`, `model`, `direction` |
| `azureclaw_upstream_retries_total` | `UPSTREAM_RETRIES` | `sandbox`, `reason` |

**AGT governance metrics:**

| Metric | Rust symbol | Labels |
|---|---|---|
| `azureclaw_agt_policy_evaluations_total` | `AGT_POLICY_EVALUATIONS` | `decision` |
| `azureclaw_agt_eval_latency_seconds` | `AGT_EVAL_LATENCY` | — |
| `azureclaw_agt_known_agents` | `AGT_KNOWN_AGENTS` | — |
| `azureclaw_agt_audit_entries_total` | `AGT_AUDIT_ENTRIES` | — |
| `azureclaw_agt_content_flags_total` | `AGT_CONTENT_FLAGS` | `category` |
| `azureclaw_agt_behavior_alerts_total` | `AGT_BEHAVIOR_ALERTS` | — |
| `azureclaw_agt_policy_rules` | `AGT_POLICY_RULES` | — |
| `azureclaw_agt_redactions_total` | `AGT_REDACTIONS` | `kind` |
| `azureclaw_agt_response_threats_total` | `AGT_RESPONSE_THREATS` | `type` |
| `azureclaw_agt_tool_rate_limits_total` | `AGT_TOOL_RATE_LIMITS` | `tool` |
| `azureclaw_agt_message_signatures_total` | `AGT_MESSAGE_SIGNATURES` | `action` |

**Handoff metrics:**

| Metric | Rust symbol | Labels |
|---|---|---|
| `azureclaw_handoff_pending_events_total` | `HANDOFF_PENDING_EVENTS` | `action` |
| `azureclaw_handoff_phase_transitions_total` | `HANDOFF_PHASE_TRANSITIONS` | `from`, `to`, `result` |

### 2.2 Token counts — not prompt text (`inference-router/src/proxy.rs`)

`record_metrics()` (line ~87) and the SSE streaming path (line ~424) both parse only:

```rust
body_json["usage"]["prompt_tokens"]
body_json["usage"]["completion_tokens"]
```

No `messages`, `choices`, `content`, or any other field carrying user text is read,
stored, or logged. This is a hard architectural constraint: the router is a
pass-through proxy; it reads only the `usage` sub-object from upstream JSON responses.

### 2.3 Structured JSON logs — `inference-router/src/main.rs`

`tracing_subscriber::fmt::layer().json()` writes structured JSON to pod stdout. Fields
logged per request (`proxy.rs` `tracing::info!` calls):

- `sandbox` (sandbox name / namespace label — no user PII)
- `model` (model deployment name)
- `status` (HTTP status code integer)
- `latency_ms` (integer)
- `resp_len` (response byte count — not content)
- `azure_request_id`, `apim_request_id` (Azure correlation ids from response headers)
- `trace_id` (16-hex internal correlation id — `main.rs` `trace_id_middleware`)

No message content, user identifiers, or IP addresses are logged on the inference path.

### 2.4 OTel GenAI SemConv constants — `inference-router/src/telemetry/gen_ai.rs`

The module defines 28 attribute-key constants and 3 metric-name constants following the
[OpenTelemetry GenAI Semantic Conventions](https://github.com/open-telemetry/semantic-conventions/blob/main/docs/registry/attributes/gen-ai.md).
The `gen_ai.rs` module itself is marked `#[allow(dead_code)]` — **no call-site in the
current codebase emits these attributes**. The security audit
`docs/security-audits/2026-04-24-phase1-otel-genai-semconv.md` explicitly documents:
"No call-site emits these attributes yet."

`OTEL_EXPORTER_OTLP_ENDPOINT` is not referenced anywhere in `inference-router/src/`.
No `opentelemetry-otlp` crate is imported. No OTLP spans leave the pod today.

### 2.5 Destination of telemetry

AzureClaw is a self-hosted operator. All components run in **the operator's AKS cluster**.

- Prometheus metrics: scraped by the operator's Prometheus instance, or
  Azure Monitor managed Prometheus if `monitoring.containerInsights: true` is set in
  `deploy/helm/azureclaw/values.yaml`. Microsoft does not receive these metrics unless
  the operator explicitly connects Azure Monitor.
- Structured logs: go to pod stdout → cluster log pipeline → wherever the operator
  routes logs. Microsoft does not receive these unless Azure Monitor Container Insights
  is configured.
- Microsoft has **no cloud-side ingestion** for AzureClaw telemetry.

### 2.6 Opt-out mechanism

**Helm (`deploy/helm/azureclaw/values.yaml`):**

```yaml
monitoring:
  enabled: false
  prometheus:
    enabled: false
  containerInsights: false
```

Setting `monitoring.prometheus.enabled: false` prevents the Prometheus `ServiceMonitor`
from being created; `/metrics` is still served but never scraped.
Setting `monitoring.containerInsights: false` prevents Azure Monitor from collecting
container logs.

**Future OTLP opt-out:** Do not set `OTEL_EXPORTER_OTLP_ENDPOINT` on inference-router
pods. When OTLP emission is wired (future release), the absence of this variable
configures a no-op exporter.

---

## 3. Threat model delta

This PR adds documentation only. No new code paths, trust boundaries, or data flows
are introduced.

| STRIDE | New exposure? | Note |
|---|---|---|
| Spoofing | No | Documentation only |
| Tampering | No | Documentation only |
| Repudiation | No | Documentation only |
| Information Disclosure | No | README clarifies data does NOT go to Microsoft by default |
| Denial of Service | No | Documentation only |
| Elevation of Privilege | No | Documentation only |

---

## 4. OSPO compliance

- Canonical OSPO Data Collection block: included verbatim in `README.md`.
- Privacy statement link: <https://go.microsoft.com/fwlink/?LinkID=824704> — present.
- Opt-out instructions: documented in README and in this audit.
- Prompt/completion text collection: explicitly disclaimed; verified from source.

---

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
