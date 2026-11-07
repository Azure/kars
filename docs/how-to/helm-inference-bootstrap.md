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
record. Records cannot be combined to manufacture coverage. Ordinary runs without
that evidence remain blocked.

### Operator-reviewed qualification Missions

When onboarding a configured harness or provider, an **operator or admin** can
select **Qualification run** in the Mission composer/review instead of changing
cluster-wide admission. The BFF independently authorizes this choice; a user or
auditor cannot obtain it by forging a request field. The optional boolean
`qualification_run` is carried on the compose, package-validation and task-create
requests, not inside the blueprint. It defaults to false and does not apply to
standing Teams.

Review the exact harness, provider/deployment, optional fallbacks, capabilities,
policies and positive finite token budget before creating the Mission. Required
mode stays enabled, including on the Kind beta profile. Missing **generic route
or selected resource execution evidence** becomes a warning for this explicitly
reviewed package. MCP servers require current schema metadata, memory requires a
backend and compiled digest for its current reconciled generation in Compiled or
Ready phase, and skills require a currently approved version. A Compiled memory
binding permits the first router execution attempt; it does not claim an already
confirmed live store.
Invalid evidence configuration remains an error. Channel qualification,
configured-runtime coverage, resource readiness, credential authority,
authentication, isolation, tool policy, independent skill approval and the durable
budget contract remain enforced. This permits a real native tool qualification
attempt; it neither adds unsupported native capabilities nor certifies a resource.

The server persists the approver's authenticated identity, approval time,
45-minute admission deadline and a SHA-256 binding of the named, namespaced
Mission's objective, envelope and reviewed blueprint. Each selected MCP, memory
and skill resource is additionally bound by namespace, name, live UID,
specification and current schema/backend/version metadata. A replacement or
change invalidates admission even when its name is unchanged; missing resources
and failed live reads fail closed. Stored validation, initial launch and new run
admission recheck these bindings. Older approvals with no resource bindings do
not acquire permission to qualify selected resources. Missions always use strict
egress: its explicit and omitted/default forms share the same binding across
Kubernetes persistence; learning mode is rejected for qualification. Allowed
destinations remain bound. Changes to the reviewed package invalidate its
authorization. Expiry blocks new admission, not a durable
run already in progress. Replicas, revisions and another attempt after completion
need a fresh separately reviewed qualification Mission; authorization is not
inherited. The designation remains visible in the Mission detail and Activity.
An accepted deliverable is not automatic route certification: inspect actual
provider/runtime observations and useful output, retain genuine task/run/artifact
evidence, add an atomic reviewed route record, and verify an **ordinary** run with
qualification disabled. No image, provider credential, budget or evidence record
is installed or created by the checkbox.

### Runtime pull coverage

Runtime preflight also needs truthful pull-coverage metadata. The existing
controller `IMAGE_PULL_REGISTRIES` environment variable, configured through
`controller.extraEnv`, lists exact registry hosts (including ports) whose pull
path the operator has verified. It does not configure authentication or mirrors.
For an anonymous local Kind registry, verify the containerd mirror on **every**
node and anonymous access to the exact pinned runtime manifests before declaring
that host. Negotiate OCI and Docker manifest media types when probing a registry.
Do not invent a pull Secret or declare a private registry covered without its
actual credential or node-identity configuration. Include this declaration in
the initial Core values so the installed native runtimes appear in Bridge.

If a first-install SRE release has already completed its ordinary ownership
transition, an upgrade may add one previously absent, nonempty literal
`IMAGE_PULL_REGISTRIES` entry to `controller.extraEnv`. The original immutable
adoption receipt is retained: removing only the new declaration must reproduce
its values digest (excluding the already-supported controller image refresh).
All other settings, existing environment entries, source identities and finite
funding remain bound. This is not permission to replace an originally sealed
registry declaration, use `valueFrom`, reset a ledger, or alter an agent pod.
Verify the exact registry pull path first and review the server-side Helm diff;
adding the metadata should change only the controller Deployment environment.

SRE authority refresh revalidates the sealed intent and any adopted catalog
transition without renewing its model contracts. An expired, unrelated route must
not block privacy readiness or sandbox creation: expiry is enforced when the
broker selects the actual provider/model/operation contract. Expired routes remain
unavailable, including to an account with an unlimited token allowance. Original
contract fields, canonical timestamps, forward-only renewals, immutable adoption
and current funding/identity checks remain enforced.

### Kind beta workflow validation

For an operator-controlled Kind beta workspace, explicitly select the existing
`validation` mode to exercise normal Mission and Team workflows without historical
route/resource evidence becoming a prerequisite for collecting that evidence.
This applies to the configured Bridge installation, not just one run; use it only
in the test workspace. The default remains `required`, including the Kind profile.
Production onboarding can continue to use per-run qualification in required mode.

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

Validation can admit MCP, memory and skills with current bound metadata but
without retained execution evidence; it does not qualify them. Channels are not
granted that exception. Live catalogue membership, runnable runtimes, credential
authority, authenticated principals, admission, budgets, tool policy and approval
requirements are unchanged. The Kind chart still requires NetworkPolicy and
rejects BFF environment overrides. Exercise the intended mixed-runtime workflows
with ample finite tool and token headroom; inspect real outputs rather than
treating successful launch as acceptance. All existing external-action controls
still apply.

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
