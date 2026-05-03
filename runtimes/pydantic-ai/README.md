# azureclaw-runtime-pydantic-ai

In-pod adapter for the [Pydantic-AI](https://ai.pydantic.dev/) agent
framework. Phase H#3 of the multi-runtime hosting initiative
(controller `RuntimeKind::PydanticAi`).

## Why a dedicated adapter (vs. BYO)

Pydantic-AI is provider-agnostic: a single `Agent` definition can
target OpenAI, Azure OpenAI, Anthropic, Gemini, etc. Each provider
SDK reads its base URL + API key from the process env at construction
time. The adapter pins each known provider base URL to the router
sidecar's matching proxy endpoint so that:

  * model calls cannot egress directly (the egress-guard init
    container drops UID-1000 packets to non-loopback / non-DNS
    targets anyway, but we belt-and-braces the pinning at the SDK
    level too);
  * API keys never live in the pod — the router substitutes the real
    AAD-attested credential on egress;
  * AGT governance, content safety, and audit run on every model
    call regardless of which provider the user picked.

## Bootstrap

```python
from azureclaw_runtime_pydantic_ai.runtime import bootstrap
bootstrap()  # idempotent — safe to call from user code or tests
```

`sandbox-images/pydantic-ai/entrypoint.sh` runs this for you before
the user agent module is imported.

## What `bootstrap()` does

1. Pins every known provider base URL to the router proxy:
   - `OPENAI_BASE_URL=http://127.0.0.1:8443/openai/v1`
   - `AZURE_OPENAI_ENDPOINT=http://127.0.0.1:8443/azure-openai`
   - `ANTHROPIC_BASE_URL=http://127.0.0.1:8443/anthropic/v1`
2. Sets each provider API-key env to the sentinel `router-managed`.
3. Initializes OpenTelemetry (GenAI semantic conventions).
4. Installs SIGTERM/SIGINT handlers for graceful shutdown.
5. Marks `__AZURECLAW_RUNTIME_INITIALIZED__=1` so re-imports no-op.

## Provenance

This package is mirrored from `runtimes/langgraph` (Phase H#2). The
multi-provider env strategy is identical because Pydantic-AI is in
the same architectural position (provider-agnostic).
