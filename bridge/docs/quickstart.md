# Private-preview quickstart

This quickstart assumes access to the private Bridge images and a compatible
Kars cluster.

## 1. Confirm Kars

```bash
kubectl -n kars-system get deploy kars-controller
kubectl -n kars-system get crd karstasks.kars.azure.com karsteams.kars.azure.com
```

Review [Compatibility](compatibility.md) before continuing.

## 2. Install Bridge with Dex

Build and load the `dev` images referenced by `values-kind.yaml`:

```bash
docker build -f bff/Dockerfile -t kars-bridge-bff:dev bff
docker build -f web/Dockerfile -t kars-bridge-web:dev web
kind load docker-image kars-bridge-bff:dev kars-bridge-web:dev --name <cluster>

helm upgrade --install kars-bridge deploy/helm/kars-bridge \
  --namespace kars-system \
  --values deploy/helm/kars-bridge/values-kind.yaml \
  --set idp.enabled=true
```

For a private remote registry, create an image-pull Secret and configure
`global.imagePullSecrets`; do not use the kind overlay’s local image names.

```bash
kubectl -n kars-system rollout status deploy/kars-bridge-bff
kubectl -n kars-system rollout status deploy/kars-bridge-web
kubectl -n kars-system port-forward svc/kars-bridge-web 3000:3000
```

Open `http://localhost:3000`.

## 3. Sign in by persona

Obtain the preview accounts from the deployment operator through a secure
channel. The chart’s sample static users are not a credential-distribution
mechanism and must be replaced or rotated for a real colleague ring.

Use one employee, operator, and auditor account. Verify:

- employee lands in Workspace;
- operator can open Workspace and Console;
- auditor lands in Audit and cannot enter Workspace or Console.

## 4. Configure a provider

In **Console → Configuration**, connect a supported provider and set a default
model. Provider credentials are operator-managed and are never returned to the
browser or agent.

## 5. Install managed MCPs

Confirm Kars has the default ToolPolicy and configured managed-MCP images, then
in **Console → Capabilities**, install:

- Playwright for a real browser integration;
- Everything for deterministic MCP conformance checks.

Everything is not a production integration. See [MCP servers](mcp-servers.md).

## 6. Run the first mission

Create a mission that:

1. uses `everything.echo` and `everything.get-sum`;
2. uses Playwright to navigate to a page and read a heading;
3. returns a structured result.

Confirm the mission page shows a deliverable, activity, MCP calls, token usage,
and retained artifacts.

## 7. Run the first team

Create a team with two differentiated roles and an objective that requires
their outputs to be combined. Run it twice and confirm the second run references
the team commons rather than starting cold.

## Next steps

- [Identity](identity.md)
- [Missions and teams](missions-and-teams.md)
- [Approvals and egress](approvals-egress.md)
- [Troubleshooting](troubleshooting.md)
