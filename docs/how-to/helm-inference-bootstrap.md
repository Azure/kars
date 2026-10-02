<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Initial inference credentials through Core Helm

For a no-Entra development installation, Core can own the initial Azure OpenAI
or Foundry API-key Secret. This requires neither `kars up` nor a separate
credential-bootstrap command or product release. Provision the inference
account and model deployment separately. AKS Agent ID qualification uses
identity authentication instead; leave these credential values empty there.

Add the endpoint and actual deployment names to the reviewed Core values:

```yaml
foundry:
  endpoint: "https://YOUR-ACCOUNT.cognitiveservices.azure.com/"
  deployments: '["gpt-4.1"]'
inferenceRouter:
  azure:
    openai:
      credentials:
        apiKey: ""
        existingSecret: ""
        key: "api-key"
```

Keep the real key in a private, mode-0600 file outside the repository. Pass it
with `--set-file`, not as a literal command-line argument. For an **existing**
Core release, preserve its complete reviewed values and explicit cluster context:

```bash
helm upgrade kars ./deploy/helm/kars --namespace kars-system \
  --kubeconfig "$KUBECONFIG" --kube-context "$CONTEXT" \
  --values /private/path/reviewed-core-values.yaml \
  --set-file inferenceRouter.azure.openai.credentials.apiKey=/private/path/inference-key \
  --wait --timeout 10m
```

For a cold install, include the same credential settings in the
[Helm-only first-install procedure](../../deploy/helm/kars/README.md#helm-only-first-install-preview).
Do not rerun the schema bootstrap just to update inference credentials.

Core creates `Secret/kars-inference-bootstrap` in its release namespace and
references its selected data key through the controller's required
`secretKeyRef`. A missing Secret/key prevents controller startup rather than
silently falling back to identity. Changing a Helm-managed key changes the
controller Pod-template checksum and requests a rollout.

Alternatively, set `existingSecret` to an operator-managed Secret in the Core
namespace and select its `key`. The chart does not create, adopt, modify, or
rotate that external Secret. The reserved name `kars-inference-bootstrap` cannot
be used as an external reference: switching ownership that way could remove the
Secret while the controller still depends on it. After rotating an external
Secret's contents, restart the controller and verify sandbox reconciliation: the
controller reads credentials at startup.
Do not set both `apiKey` and `existingSecret`, or duplicate
`AZURE_OPENAI_API_KEY` through `controller.extraEnv`.

## Home composition model compatibility

Home composition uses the current Chat Completions `max_completion_tokens` limit
for both the governed router path and an explicit operator endpoint. The limit
covers reasoning and visible output, including when an InferencePolicy selects a
reasoning deployment behind a model alias. Native Anthropic Messages requests
retain `max_tokens`. Router selection excludes terminating Pods during rollout;
readiness alone does not make a deleting Pod a usable inference target.

## Fresh-install route validation

Bridge defaults to `bff.routeQualification.mode: required`: a complete runtime,
model, capability and parallelism contract must match one retained qualification
record. Records cannot be combined to manufacture coverage. Fresh installations
without evidence cannot launch in this mode.

For a reviewed development qualification run, explicitly configure Bridge:

```yaml
bff:
  routeQualification:
    mode: validation
    recordsJson: ""
```

Apply these values with the existing Bridge Helm release and its complete reviewed
values. Validation mode permits execution without historical **generic route**
evidence; it does not generate evidence or mark the route qualified. Composition
rationale discloses the mode, and preflight reports an unqualified route as a
**warning**, including fallback routes. Automatic fallback candidates obey the
same admission policy. Invalid configured evidence still fails closed.

This does **not** relax current-digest proof for selected MCP servers, memory,
skills or channels. Live catalogue membership, runnable runtimes, credential
authority, authenticated principals, admission, budgets, tool policy and approval
requirements are unchanged. A text-only mission requiring none of those optional
resources is an appropriate first validation run. All existing external-action
controls still apply.

Retain genuine task/run/artifact evidence after actual execution, then provide
reviewed route records using
`--set-file bff.routeQualification.recordsJson=/private/path/route-records.json`
and switch `mode` back to `required`. Verify the required-mode mission again.
The file must be a JSON array of the existing `QualifiedRoute` format; no enrollment
or certification is implied by this setting. Records and their task/run/digest
references must come from actual verified execution, not synthetic values.

For existing installations, `BRIDGE_QUALIFICATION_RECORDS_JSON` in `bff.extraEnv`
remains supported when `recordsJson` is empty. Do not supply both sources. Configure
the mode only through `bff.routeQualification.mode`, not `extraEnv`. Neither a
warning nor a successful launch passes a beta gate: useful attributable delivery,
linked evidence, feedback, persistent-team behavior and required-mode checks remain
acceptance work.

## Security and qualification boundaries

- `--set-file` avoids command-line disclosure, **not** Helm release-history
  storage. Helm-managed keys remain in release values/manifests and Kubernetes
  Secret storage. Restrict access, use encryption at rest, and follow the
  operator's secret-retention/rotation policy. Never publish rendered private
  manifests or `helm get values` output.
- This uses the existing development router credential path. The controller
  currently copies the key into the **router container's literal environment in
  generated sandbox Deployment/Pod specifications**. It is not injected into
  the agent container, but identities able to read those workload specifications
  can see it. This is not a production secret-isolation guarantee or a substitute
  for per-agent Entra authentication.
- Initial operator-owned Helm configuration does not enroll a Bridge credential
  store, create a `KarsCredentialGrant`, or authorize provider changes through
  Bridge. Home can discover and use the controller's configured model catalogue
  without enrolling additional providers. An absent optional provider enrollment
  contributes no extra models; broken existing authority still fails closed.
  Governed operator enrollment remains required for provider changes.
- Successful installation or controller rollout does not prove inference. Verify
  sandbox readiness and a real model response through its router, followed by
  useful agent delivery and linked evidence.
