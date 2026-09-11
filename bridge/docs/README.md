# Kars Bridge documentation

Kars Bridge turns Kars APIs into a governed human workflow for agent missions
and teams. This documentation is organized by task and persona rather than by
repository component.

## Get started

1. [Compatibility and prerequisites](compatibility.md)
2. [Private-preview quickstart](quickstart.md)
3. [Identity and sign-in](identity.md)
4. [First mission and first team](missions-and-teams.md)
5. [Glossary](glossary.md)

## Employee and team workflows

- [Missions and teams](missions-and-teams.md)
- [Team workflows: intent to reviewed outcome](team-workflows.md)
- [Connections](connections.md)
- [Skills](skills.md)
- [Approvals and egress](approvals-egress.md)

## Operator workflows

- [MCP servers](mcp-servers.md)
- [Providers and model routing](providers.md)
- [Providers and local inference](local-inference.md)
- [Inference budgets](inference-budgets.md)
- [Access and roles](rbac.md)
- [Observability and evidence](observability.md)
- [Evidence and compliance](evidence-compliance.md)

## Deploy and operate

- [Deployment](deployment.md)
- [Compatibility](compatibility.md)
- [Troubleshooting](troubleshooting.md)
- [Operations](operations.md)
- [Architecture](architecture.md)

## Product model

| Surface | Primary user | Responsibility |
|---|---|---|
| Workspace | Employee | Create and review missions/teams, connections, and deliverables |
| Operator Console | Operator/admin | Govern the fleet and integrations |
| Audit | Auditor/admin | Verify retained evidence without mutation rights |

Bridge is additive. Kars remains independently installable and usable.
