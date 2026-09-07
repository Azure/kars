# Local inference and model failover

Kars can route to operator-configured OpenAI-compatible endpoints alongside
existing Azure OpenAI, Foundry, GitHub Models, Copilot, and typed Ollama or
Anthropic providers. No model or infrastructure is deployed automatically.

## Configure additional providers

The optional `kars-inference-providers` Secret in the controller namespace
contains router environment variables:

| Variable | Meaning |
|---|---|
| `KARS_PROVIDER_<TAG>_ENDPOINT` | Provider's complete API base URL |
| `KARS_PROVIDER_<TAG>_API_KEY` | Optional credential for that provider |
| `KARS_PROVIDER_<TAG>_TOKEN` | Alternative bearer credential; API key takes precedence |

For example, `KARS_PROVIDER_LOCAL_ENDPOINT` maps to provider tag `local`.
Tags are case-insensitive; underscores in environment names become hyphens.
The controller mirrors this Secret into sandbox namespaces and exposes it only
to the inference-router container, never the agent. Secret revisions cause the
next sandbox reconciliation to refresh the pod's environment.

Use a base URL including `/v1` for servers such as vLLM or llama.cpp that serve
`/v1/chat/completions`. Azure-owned endpoints retain the existing `/openai/v1`
prefix behavior. Custom endpoints do not receive that Azure prefix. Typed
Ollama routes retain their `/v1` translation and receive no credential.

Services under `*.kars-local-inference.svc.cluster.local` receive no
authentication credential, including on Workload Identity clusters.
An arbitrary non-Azure endpoint must have an explicitly configured credential
when ambient Workload Identity/IMDS would otherwise be used. Additional provider
configuration does not change the existing default endpoint.

## Allow local model traffic

Local-model egress is opt-in. Prefer a narrowly scoped Helm value:

```yaml
localInference:
  targets:
    - namespace: kars-local-inference
      matchLabels:
        app: inference-model
      ports: [5000]
```

Use the model pod's destination port: NetworkPolicy is commonly evaluated after
Service DNAT. Targets require a namespace, nonempty pod labels, and TCP ports in
`1..65535`; malformed targets fail reconciliation rather than broadening access.
Explicit targets replace any namespace-wide allowances.

For operators intentionally trusting all model services in a namespace,
`localInference.namespaces` accepts namespace names instead. Its default is
empty. Existing default-deny, agent UID isolation, Content Safety, prompt
shields, policy floors, and declared guardrail pipelines remain enforced.

## Select primary and fallback routes

An `InferencePolicy` selects an ordered route chain:

```yaml
modelPreference:
  primary:
    provider: local
    deployment: primary-model
  fallback:
    - provider: foundry
      deployment: fallback-model
```

For a `KarsTask` or a team blueprint, use `model` and `modelFallbacks`:

```yaml
blueprint:
  model:
    provider: local
    deployment: primary-model
  modelFallbacks:
    - provider: foundry
      deployment: fallback-model
```

The new fallback list permits up to eight nonblank provider/deployment pairs.
Exact duplicate pairs and repeats of the primary are removed without changing
the remaining order. Existing primary route fields and team roster sizes are
not restricted by the new field's limits.

The router tracks health per provider/deployment and tries another candidate
on transport failures, HTTP 429, or HTTP 5xx. Ordinary client/auth/policy errors
are returned rather than retried. The original default route is retained as a
safety net. An unavailable-model response can additionally recover once to the
configured default model; capability caches are scoped to the endpoint/model.

Streaming failover ends as soon as a provider returns a successful response.
A later stream failure is never replayed onto another provider, preventing
duplicate generations. Responses-only model recovery retains the selected
provider and effective deployment.

Provider families must support the request API. The public typed Anthropic
provider continues to use `/anthropic/v1/messages`; Kars does not silently
translate OpenAI chat requests into Anthropic requests.

No credentials, private registries, customer deployment values, or hardware
requirements are needed in a public model-routing configuration.
