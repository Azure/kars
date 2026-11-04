<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Providers and model routing

Bridge separates provider connections from per-mission model policy.

## Connection model

- One provider-qualified model is the saved default for new missions and teams.
  Changing it does not rewrite controller bootstrap settings or existing explicit
  orchestrator, SRE, mission, or team routes.
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

Named connections and the saved-default annotation reuse the UID-enrolled
`kars-inference-providers` Secret (`providers` purpose) in the Core namespace.
Adding another provider or changing the saved default does not require another
Secret or another per-provider grant. The workspace grant must be Ready and
bound to the workspace UID and actual BFF ServiceAccount writer identity.

The separate, pre-private-activation controller-bootstrap action additionally
requires `kars-credential-controller-settings` (`controller-settings` purpose)
and the enrolled controller Deployment UID. API-key bootstrap writes reuse
`KARS_PROVIDER_<TAG>_API_KEY` in `kars-inference-providers`; the controller setting
references that exact enrolled Secret UID/key. New `kars-provider-<tag>` /
`provider-default` stores are not required. Existing enrolled legacy stores remain
supported for their existing consumers. Sealed private workspaces still reject
controller-bootstrap changes; use named connections and the Model catalogue for
post-install defaults. Connecting separate Foundry project services still uses
the Foundry integration's own enrolled credentials; a named inference endpoint
alone does not enroll those services.

## Operator workflow

1. Open Console → Configuration → Connect a provider.
2. Select the provider type; after installation, add a named connection rather
   than replace controller bootstrap configuration.
3. Authenticate or provide the endpoint/key. Azure Workload Identity connections
   may leave the key blank. GitHub Models requires a token for a new connection.
4. Discover models where supported, or enter deployment IDs, then save.
5. In the Model catalogue, choose **Set as default** on the desired provider/model.
6. Use **Edit models** on a connected provider (including the default row) to
   update its models, endpoint or key. Copilot reuses its stored sign-in;
   endpoint-based connections preserve stored credentials when the key is blank.
   Tags are stable during edits. Foundry project and local inference retain their
   dedicated connection/deployment controls.
7. Before removing a provider or deselecting a used model, select a replacement
   default and update any referencing primary/fallback routes in Console →
   Policies. In particular, inspect `bridge-orchestrator-inference` and the SRE
   inference policy. Controller-owned mission policies must be handled through
   their owning workflow, not forcibly edited. Removal rejects known references
   with an actionable conflict; it never silently moves them to another provider.
   Shared keys referenced by controller bootstrap settings are also protected:
   replace that bootstrap binding before private activation, or retain the key.
   Selecting a new mission default does not release a controller bootstrap key.
8. Remove the now-unused connection (or use Foundry Disconnect / local-model
   undeploy). These dedicated removal paths also preflight provider usage.
9. Run a fresh mission and verify the actual provider/model in telemetry; check
   orchestrator composition and SRE separately after changing their explicit routes.

## Failure behavior

- Missing credentials fail preflight or the router request.
- Removing a connection or model requires a successful cluster-wide policy read.
  An unavailable or missing policy API blocks the change without writing provider
  credentials; it is not treated as an empty policy list.
- Responses-only models use the router’s Responses API path.
- An unavailable model may use only the declared fallback/default behavior.
- Provider Secrets are mirrored at sandbox creation; connection changes may
  require new sandbox pods.

See [Local inference](local-inference.md) and
[Inference budgets](inference-budgets.md).
