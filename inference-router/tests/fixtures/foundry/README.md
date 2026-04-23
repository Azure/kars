# Sanitized Azure / Foundry fixtures

These JSON files are **sanitized copies** of real Azure AI Foundry / Azure OpenAI
responses, used by `inference-router/tests/` to exercise the proxy end-to-end
against a fake upstream (`tests/common/mod.rs`).

**No real subscription IDs, tenant IDs, resource group names, project names,
user prompts, or user responses are included.** Every such field is replaced
with `REDACTED` or a generic placeholder.

## Recording a new fixture

1. Run the router against a dev Azure OpenAI or Foundry project and capture
   the raw upstream response (e.g. with `tracing::debug!` or `RUST_LOG=hyper=trace`).
2. Save to `tests/fixtures/foundry/<name>.json`.
3. Scrub the following before committing:
   - `id` / `chatcmpl-*` / `resp_*` / `msg_*` IDs → replace with `REDACTED<N>`.
   - `system_fingerprint` → `fp_REDACTED`.
   - Any `subscription_id`, `resource_group`, `tenant_id`, `project_name`,
     account names, endpoint hostnames in the response body.
   - Any `"content"` or `"text"` originating from user prompts or model
     completions → replace with a short placeholder sentence.
4. Keep the structural shape exactly: field names, ordering, nesting — the
   router's parsers (`safety.rs`, `proxy.rs::record_metrics`) depend on the
   schema, not the content.

## Fixture index

| File | Upstream shape | Notes |
|------|----------------|-------|
| `chat_completion_ok.json` | `POST /openai/v1/chat/completions` | Successful completion + prompt_filter_results + usage |
| `chat_completion_filtered.json` | same | Jailbreak + hate filter triggered; `finish_reason: content_filter` |
| `embeddings_ok.json` | `POST /openai/v1/embeddings` | Minimal 5-dim vector |
| `models_list.json` | `GET /openai/v1/models` | Three models |
| `responses_ok.json` | `POST /openai/v1/responses` | Responses API reply with output array |
| `connections_list.json` | `GET /connections` | Azure OpenAI + Bing connection |
| `memory_stores_empty.json` | `GET /memory_stores` | Empty list |
| `error_429.json` | any | Rate-limit error shape |

## Why in-tree?

Live Azure tests are slow, flaky, and leak cost. These fixtures let every
router test exercise the full `proxy::forward` → IMDS → upstream → response
path on an air-gapped developer machine in milliseconds.
