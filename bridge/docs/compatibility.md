# Compatibility

Bridge is version-coupled to Kars APIs, but is not a required core component.
Deployments must record the qualified Kars and Bridge source revisions and
resolved image digests.

## Current publication boundary

The complete application lives under `bridge/` in **Azure/kars**, with
publication PRs targeting **`kars-bridge`**. This is an integration preview,
not a release or a recommendation to install an unqualified candidate.
Publishing source does not publish images or qualify all product workflows.

Kars core builds, installs, runs and upgrades without Bridge. Bridge retains
its own Rust/npm packages, lockfiles, container images and Helm release.
Installing or removing Bridge must preserve core resources and customer data.
The existing `/sandbox` filesystem is ephemeral (`emptyDir`); source publication
does not introduce persistent agent workspaces.

Normal core CI remains independent of application builds. Separate Bridge CI
qualifies its components and add-on lifecycle. Native qualification builds core
and BFF from the **same immutable monorepo commit**; `CORE_REVISION` must equal
the checked-out commit. Both real API/admission and runtime/TLS/CNI jobs must
pass. A green component build or API readiness probe cannot replace them.

## Qualification status

The import is not yet release-qualified. Historical private-preview runs used
different source and images; they are not acceptance evidence for this
monorepo candidate.

| Platform | Current candidate |
|---|---|
| AKS | No same-candidate end-to-end qualification yet |
| Local kind | Component/chart regressions available; native acceptance required |
| EKS | No same-candidate end-to-end qualification |
| GKE | No same-candidate end-to-end qualification |
| Other Kubernetes | Untested |

## Release requirements

Application source can be reviewed while dependent core PRs are qualifying.
Before merging the application candidate, its core dependencies, application
checks, security reviews and same-candidate native gates must all be complete.
Do not bypass failed gates to make the import appear release-ready.

The complete standing-Team journey additionally requires the compatible
governed-delivery producer and authenticated runtime transport: proposal,
approved launch, recurring work, per-agent evidence, review, revision and
restart recovery. Those behaviors are not established merely by importing
their Bridge consumers. See [Team workflows](team-workflows.md) for the product
contract, not a claim that every path is qualified on this integration branch.

Each release must record the exact qualified source commit, resolved component
image digests, runtime/provider configuration, platform and acceptance outcomes.
The broad chart version alone is not a compatibility guarantee. Registry
publication, deployment and promotion to `main` are separate from integration
source publication.

The chart retains legacy preview image repository defaults for configuration
compatibility. These are not advertised public artifacts. Build the application
images and set operator-controlled repositories explicitly as described in
[Deployment](deployment.md).

## Required Kars capabilities

Bridge requires the Kars APIs and controller behavior for:

- `KarsSandbox`, `KarsTask`, `KarsTeam`, `KarsProfile`, `KarsSkill`;
- `KarsApproval`, `EgressApproval`, `KarsReceipt`;
- managed and external `McpServer`;
- `InferencePolicy`, `ToolPolicy`, `KarsMemory`, `KarsEval`, and `KarsSREAction`;
- team commons, task retention, runtime selection, and parent-scoped spawn;
- router telemetry and task artifacts.

Installing against an older public Kars release may render successfully but
fail at runtime if those APIs or fields are absent. The BFF readiness probe
therefore performs a read-only, limited list against each required
`kars.azure.com/v1alpha1` API in the configured namespace before the BFF pod
becomes Ready. Missing APIs, missing list permission, authentication errors,
throttling, server/transport errors, invalid responses, and a five-second total
timeout all produce HTTP 503. Kubernetes gives this probe ten seconds;
`/healthz` remains independent process liveness. Detailed failures are logged
server-side, not exposed in the readiness body.

This is a necessary API/RBAC guard, **not proof of controller health, schema
fields, or behavioral compatibility**. Every release still needs the exact
qualified Kars commit and image digests; offline chart and readiness regressions
do not replace that runtime qualification.

Microsoft Teams is optional: its gateway defaults to zero replicas and missing
tenant credentials do not block a compatible Kars-backed web deployment.
Teams credentials are not readiness prerequisites. Local development without
any Kars connection can serve `/healthz`, but correctly remains unready.

## What “portable” means

The Bridge workloads themselves use standard Deployments, Services,
NetworkPolicies, Secrets, RBAC, and optional Ingress. Cloud integration is
still environment-specific:

- image registry and pull identity;
- ingress controller and TLS;
- Kars inference identity;
- CNI NetworkPolicy behavior;
- local-inference stack;
- storage and observability.

Do not claim EKS/GKE support until the full identity, MCP, mission, team, and
failure-recovery matrix has run live on those platforms.
