# Providers and model routing

Bridge separates provider connections from per-mission model policy.

## Connection model

- One provider/model can be the cluster default.
- Additional providers are mirrored to sandbox routers.
- `InferencePolicy.modelPreference.primary.provider` selects the provider for a
  specific sandbox or task.
- Fallbacks are explicit; a missing model must not silently change provider.

Supported private-preview paths include Azure AI Foundry/Azure OpenAI, GitHub
Copilot, GitHub Models, OpenAI-compatible endpoints, and AI Runway local
inference.

## Credential handling

Provider credentials are stored in canonical Kubernetes Secrets and read by the
BFF/router as required. The UI reports connection metadata such as provider,
endpoint, and whether a key exists; it does not return credential values.

## Operator workflow

1. Open Console → Configuration.
2. Select a provider type.
3. Authenticate or provide the required endpoint/secret.
4. Run live model discovery.
5. Choose default or additional scope.
6. Set the default model.
7. Run a mission and verify the actual provider/model in telemetry.

## Failure behavior

- Missing credentials fail preflight or the router request.
- Responses-only models use the router’s Responses API path.
- An unavailable model may use only the declared fallback/default behavior.
- Provider Secrets are mirrored at sandbox creation; connection changes may
  require new sandbox pods.

See [Local inference](local-inference.md) and
[Inference budgets](inference-budgets.md).
