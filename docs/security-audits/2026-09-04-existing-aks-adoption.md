# Security Audit — Existing AKS CLI adoption

Date: 2026-09-04
Scope: `cli/src/commands/config.ts`, `docs/how-to/helm-installation.md`.
Gated paths: `cli/src/commands/config.ts`.

## Summary

Adds an explicit command that registers metadata for an existing,
Helm-installed AKS cluster in the local Kars deployment context. The command
does not create, update, or delete Azure or Kubernetes resources.

## T1: New capability / attack surface? (YES)

- Adds `kars config adopt-aks`.
- Reads the selected cluster's Kars CRD and Helm release.
- Writes `~/.kars/context.json`, which existing lifecycle commands already use.

## T2: Security-control change? (NEUTRAL)

- Cluster selection remains explicit through `--context` or the user's current
  kube context.
- The command verifies both the `KarsSandbox` CRD and `kars` Helm release before
  persisting metadata.
- Registry input is restricted to Azure Container Registry hosts.

## T3: Availability / fail-open risk? (REDUCED)

- Helm-installed clusters no longer require hand-written context files.
- Missing cluster access, CRDs, or Helm ownership fail before context is saved.

## Verification

- CLI typecheck, focused tests, package build, LOC, and repository security
  gates.

## Verdict

Accept. The command is read-only against infrastructure and makes existing
cluster targeting explicit and auditable.

Signed-off-by: Pal Lakatos-Toth <pallakatos@github.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
