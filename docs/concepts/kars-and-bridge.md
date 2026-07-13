# Kars and Kars Bridge

Kars and Kars Bridge are separate products with a one-way dependency.

| | Kars | Kars Bridge |
|---|---|---|
| Role | Secure agent runtime and Kubernetes APIs | Human-facing mission control |
| Repository | Public OSS | Private/incubating today |
| Required for the other product | Runs independently | Requires a compatible Kars cluster |
| Primary users | Platform engineers, runtime authors, GitOps operators | Employees, operators, administrators, auditors |
| Interfaces | CRDs, controller, router APIs, CLI, runtime plugins | Workspace, Operator Console, Audit surface, BFF API |

## Hard boundary

**Bridge depends on Kars; Kars never depends on Bridge.**

Every primitive Bridge uses must remain independently usable on a plain Kars
cluster:

- missions are `KarsTask` resources;
- standing teams are `KarsTeam` resources;
- skills, approvals, receipts, memory, MCP servers, and policies are Kars APIs;
- Bridge composes, validates, and presents those APIs.

Kars documentation must not require a reader to have Bridge access. Bridge
documentation may link to the public Kars substrate and must state the exact
compatible Kars version or commit used by a private-preview release.

## When to use each

Use Kars directly when you want:

- GitOps-managed agent sandboxes;
- a framework/runtime integration;
- custom control-plane automation;
- a minimal open-source deployment without the product UI.

Use Bridge when you want:

- plain-language mission and team composition;
- employee, operator, and auditor personas;
- human approval workflows and inboxes;
- visual evidence, receipts, budgets, MCP, skills, and fleet operations.

## Compatibility

Bridge evolves alongside Kars APIs. A Bridge release must publish:

- the compatible Kars version or commit;
- required CRDs and minimum schema versions;
- controller/router/runtime image digests;
- required Kubernetes version;
- migrations and known limitations.

See the Kars [compatibility matrix](../reference/compatibility.md) and the
Bridge compatibility document in the Bridge repository.
