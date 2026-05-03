# Security Reviewer Roster

Independent reviewers qualified to sign off on security-sensitive changes per
`docs/security-audits/_template.md` §11 ("Independent reviewer sign-off").

## Roster governance

- **Maintainer:** TBD (governance pending repo maintainer decision).
- **Review SLA:** TBD.
- **Fallback when primary reviewer is unavailable:** TBD.
- **Addition/removal policy:** TBD.

This section is a placeholder until leadership confirms governance. Until then,
any two-engineer review by maintainers of the AzureClaw Controller / Router /
CLI is acceptable for non-router-data-plane capabilities; router-data-plane,
admission-policy, and sandbox-image changes require a named reviewer below.

## Reviewer roster (placeholders — to be populated)

| Handle | Area of depth | Backup for |
|---|---|---|
| _(populate before first Phase 1 merge)_ | | |

## Scopes that require a named roster reviewer

Per principle §0.2 #9 of the implementation plan:

- Anything under `controller/src/admission/**`
- Anything under `controller/src/reconcilers/**` that changes a trust or
  identity-binding surface (`fedcred.rs`, `pairing.rs`, `mesh_peer.rs` today)
- Anything under `inference-router/src/{mcp,a2a,providers,routes}/**`
- Anything under `runtimes/openclaw/src/core/**` or
  `runtimes/openclaw/src/index.ts` (in-sandbox runtime adapter)
- Anything under `sandbox-images/*/Dockerfile`, `entrypoint.sh`,
  `cli/profiles/**`, `deploy/seccomp/**`, `deploy/helm/azureclaw/files/**`
- Vendored patch updates (`vendor/*/src/**`, `vendor/*/dist/**`)

## Scopes that accept any maintainer as second signer

- CLI commands that do not manipulate secrets/keys
- Migration adapters
- Docs-only changes
- Test additions (though the corresponding capability's audit doc still
  needs a roster reviewer when in scope above)
