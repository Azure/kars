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
| `KARS_PROVIDER_<TAG>_TOKEN` | Alternative provider credential; API key takes precedence |

For example, `KARS_PROVIDER_LOCAL_ENDPOINT` maps to provider tag `local`.
Tags are case-insensitive; underscores in environment names become hyphens.
The controller mirrors this Secret into sandbox namespaces and exposes it only
to the inference-router container, never the agent. Secret revisions cause the
next sandbox reconciliation to refresh the pod's environment.

Use a base URL including `/v1` for servers such as vLLM or llama.cpp that serve
`/v1/chat/completions`. Azure-owned endpoints retain the existing `/openai/v1`
prefix behavior. Custom endpoints do not receive that Azure prefix. Typed
Ollama routes retain their `/v1` translation and receive no credential.

Each named route uses only its own credential, or no authentication when that
credential is absent. It never borrows the default API key, Workload Identity,
IMDS, or sidecar identity. This also applies to model Services in arbitrary
Kubernetes namespaces. The legacy default route's authentication behavior and
its dedicated `kars-local-inference` namespace exclusion remain unchanged.

For a named **Copilot** endpoint, the credential must be a GitHub OAuth/seat
token accepted by Copilot, not an Azure/OpenAI API key or an already-exchanged
inference JWT. The router performs Copilot's exchange and maintains a separate
cache for each selected provider identity/credential. Missing named Copilot
credentials fail before inference HTTP; they cannot borrow another account.
The global `COPILOT_GITHUB_TOKEN` cache is reserved for the legacy default route.

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
Older installations using Helm `--reuse-values` may have no `localInference`
section at all. Missing sections or lists remain empty and keep local egress
disabled, without replacing customer values.

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

An explicit `InferencePolicy.spec.provider` remains authoritative for the
primary candidate, even if `modelPreference.primary.provider` contains a
conflicting informational tag. Each fallback keeps its own provider, and the
true legacy default remains a separate final candidate.

Without that authoritative field, the existing primary provider label remains
metadata unless its named endpoint is separately registered. For example,
`anthropic` / `claude-prod` can continue using the default Azure/Foundry route.
Merely supplying a global native-provider key or Ollama URL does not change
that intent. Metadata and explicit native routes have separate health identities
when the same label can select different backends.

The router tracks health per provider/deployment and tries another candidate
on connection failures known to precede acceptance, HTTP 429, or HTTP 5xx.
Authentication/configuration acquisition failures and ambiguous transport
errors are not retry triggers. Ordinary client/auth/policy errors are returned
rather than retried. An unavailable-model response can additionally recover
once to the configured default model; capability caches include the immutable
provider identity as well as endpoint/model, never credential text. Credential
updates roll the sandbox process and its capability caches.

Known 429/5xx rejections remain retryable if their bodies truncate after headers.
That is distinct from an accepted 2xx response, an ordinary 4xx rejection, or an
ambiguous connection loss; those body/transport failures are never replayed.

Both buffered and streaming failover end when successful response headers are
accepted. A later body failure is never replayed onto another provider,
including the chat-to-Responses recovery path. Responses-only model recovery
retains the selected provider and effective deployment.

Provider families must support the request API. The public typed Anthropic
provider continues to use `/anthropic/v1/messages`; Kars does not silently
translate OpenAI chat requests into Anthropic requests.

No credentials, private registries, customer deployment values, or hardware
requirements are needed in a public model-routing configuration.
