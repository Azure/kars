# Operations

Bridge is stateless application code over Kubernetes-resident Kars resources.
Production operation still requires explicit SLOs, scaling, backup, and
incident procedures.

## Operate these components

- web Deployment and Service;
- BFF Deployment, ServiceAccount, and RBAC;
- OIDC provider and signing secrets;
- ingress and TLS;
- Kars controller/router/runtime compatibility;
- mission/team resources and retained ConfigMaps;
- provider, GitHub, channel, and MCP credentials.

## Minimum operational controls

- availability alerts for web, BFF, controller, AgentMesh, and required MCPs;
- latency/error dashboards for BFF and mission launch;
- audit of RBAC and Secret access;
- image and dependency vulnerability monitoring;
- secret rotation runbooks;
- Helm upgrade and rollback rehearsal;
- evidence export and retention;
- capacity planning for sandboxes, browsers, and GPU models.

## Scaling

The preview chart defaults to one web and one BFF replica. Before increasing
replicas, verify:

- session signing keys are shared;
- BFF operations remain idempotent/CAS-protected;
- ingress health and readiness are correct;
- topology spread and disruption budgets fit the cluster.

## Backup

Bridge source-of-truth data lives in Kars CRDs, Secrets, and ConfigMaps. Back up
the Kubernetes API objects and any external evidence store according to your
cluster’s supported backup mechanism. Never export credential Secret values
into support bundles.

## Incident response

Use [Troubleshooting](troubleshooting.md), preserve trace IDs and receipts,
rotate affected credentials, and use Kars emergency-stop/break-glass controls
only through the documented audited path.
