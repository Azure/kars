# Security audit — Phase 1 · OTel GenAI SemConv constants

Audit ID: `2026-04-24-phase1-otel-genai-semconv`  
Scope reference: `docs/implementation-plan.md` §7 item 9
(OTel GenAI SemConv 1.x emission on every router span).

## What landed

1. `inference-router/src/telemetry/mod.rs` — new module tree.
2. `inference-router/src/telemetry/gen_ai.rs` — pure-data module:
   * 28 attribute-key constants frozen against the upstream GenAI
     semantic-conventions registry (retrieved 2026-04-24).
   * 3 metric-name constants (`gen_ai.client.operation.duration`,
     `gen_ai.client.token.usage`, `gen_ai.server.request.duration`).
   * `Operation` enum with `as_str()` / `from_wire()` for the seven
     canonical `gen_ai.operation.name` values (chat, text_completion,
     embeddings, generate_content, execute_tool, create_agent,
     invoke_agent).
   * `TokenType` enum (`input`, `output`) for the token-usage metric.
   * `systems::{OPENAI,AZURE_AI_INFERENCE,…}` constants covering the
     eleven most-common providers.
   * `GenAiAttributes<'a>` typed attribute bag (zero owned allocations
     on fields) with `to_rendered()` helper that emits `(key, value)`
     pairs, omitting absent fields.
3. `inference-router/src/lib.rs` — `pub mod telemetry;` wired in.

**9 unit tests** pin every attribute-key string against the spec value,
operation/token-type canonical strings, system constants, and the
rendering helper's omit-absent-fields contract.

No call-site emits these attributes yet. That wiring (plan §7 item 9)
lands with the MCP/A2A decomposition pass. Landing the constants first
means every emission site references *one* source of truth; a typo in a
span call at the edge can't silently corrupt the wire format.

## STRIDE

| Category | Applies | Note |
|---|---|---|
| **Spoofing** | N/A | Pure string constants. No identity surface. |
| **Tampering** | Partial | Attribute keys are module-level `pub const &str` — immutable, compile-time. `Operation::from_wire` is a closed `match` — unknown wire values return `None`, never coerced. |
| **Repudiation** | Positive | Standardising attribute names is the whole point — traces and metrics from the router are self-describing and comparable across providers. |
| **Information Disclosure** | **Yes** | The spec recommends attributes for `gen_ai.prompt` / `gen_ai.completion` / `gen_ai.input.messages` / `gen_ai.output.messages` that carry *raw user content*. Those attributes are **deliberately not emitted** from this module. Rendering prompts into traces violates plan §0.2 #9 without an explicit PII-redaction pass first. Future PR proposing any of those keys MUST ship alongside a redactor component and its own audit doc. |
| **Denial of Service** | N/A | Pure data; constant-time rendering. |
| **Elevation of Privilege** | N/A | No auth surface. |

## Principle mapping

* §0.2 #1 — zero regressions: net code addition only. `cargo test --all`
  went 305 → 314 passed (+9).
* §0.2 #4 — LOC: `gen_ai.rs` 430 lines of which ~160 are doc/tests; well
  under the 800-line default cap for a freshly-added module.
* §0.2 #5 — language: Rust, as policy for router modules.
* §0.2 #8 — fail-closed: `Operation::from_wire` refuses unknown values.
  `GenAiAttributes::to_rendered` emits nothing on default — a caller
  that forgot to populate fields doesn't ship a lie into a dashboard.
* §0.2 #9 — this audit doc.
* §0.2 #10 — references pinned:
  * Registry table:
    `open-telemetry/semantic-conventions/docs/registry/attributes/gen-ai.md`
    (consumed 2026-04-24 via raw.githubusercontent.com).
  * Spans doc: `open-telemetry/semantic-conventions/docs/gen-ai/spans.md`.
  * Metrics doc: `gen_ai.client.token.usage`, `gen_ai.client.operation.duration`
    names confirmed at `opentelemetry.io/docs/specs/semconv/gen-ai/gen-ai-metrics/`.

## What was **not** done (deliberate)

* No span emission: no `tracing::Span::record` call-sites. Consumer
  wires these up in the MCP/routes module; this module only provides
  the constants. Avoiding a pseudo-"emit here someday" wrapper per
  §0.2 #8.
* No `opentelemetry::KeyValue` coupling: the call-site chooses how to
  emit — plain `tracing` fields, OTel SDK `KeyValue`, or a histogram
  label. Coupling here would force every consumer to link in the same
  OTel crate version.
* No `gen_ai.prompt` / `gen_ai.completion` / `gen_ai.input.messages` /
  `gen_ai.output.messages` / `gen_ai.input.text` constants. Those carry
  raw content and need a companion redactor. Landing the constant
  without the safety mechanism is the kind of "pseudo-control" the
  principles explicitly reject.
* No attribute catalogue for `gen_ai.evaluation.*`. Evaluation
  telemetry is a Phase 2/3 scope (ClawEval CRD); adding constants here
  ahead of the consumer violates §0.2 #8.

## Re-audit triggers

* Upstream spec promotes any of these from *Development* to *Stable* —
  verify string values haven't been renamed, refresh citations.
* Any proposal to add prompt/completion-content attribute constants →
  full re-audit with a companion redactor PR.
* Any new `gen_ai.system` enters `systems::` constants → verify spelling
  against the upstream `gen_ai.system` attribute examples.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
