# Security Audit Records

Every capability-introducing PR ships with a security-audit record in this
directory — enforced by `ci/security-audit-required.sh`.

## What is a "capability" for audit purposes?

Any change to:

- CRDs (`controller/src/crd/**`)
- Reconcilers (`controller/src/reconcilers/**`)
- Admission policies (`controller/src/admission/**`)
- Router modules (`inference-router/src/{mcp,a2a,providers,routes}/**`)
- CLI commands / adapters / migrations
  (`cli/src/{commands,migrate,adapters}/**`)
- Sandbox images (`sandbox-images/*/Dockerfile`, `entrypoint.sh`)
- Sandbox seccomp/policy profiles (`cli/profiles/**`)
- Vendored patch updates (`vendor/*/src/**`, `vendor/*/dist/**`)

## Workflow

1. Copy `_template.md` to `<YYYY-MM-DD>-<slug>.md` in this directory, e.g.
   `2026-04-24-mcp-streamable-http.md`.
2. Fill every section. "N/A" is acceptable — with a one-line justification.
3. Author signs Section 11 sign-off block.
4. A second engineer reviews. For router data-plane, sandbox-image, or
   admission-policy changes, the second signer must be from the roster in
   `docs/security-reviewers.md`.
5. Commit alongside the capability code in the same PR.
6. `ci/security-audit-required.sh` blocks merge unless the file exists.

## Why this exists

We have been bitten by "looks like it works" bugs — base64 wrappers
pretending to be Signal, routers returning 200 without running Content Safety,
prekey bundles with empty signatures nobody noticed. Security audit records
force an explicit answer to *how do we know the control actually ran?* before
the capability ships.

## Non-goals

This is NOT a CAB. It is not a bottleneck. Two engineers, a completed
template, and the PR moves. If the doc is trivially fillable, that is a signal
the capability is well-scoped. If it is not, that is a signal the design needs
more thought before code is written.

## References

- `docs/security-audits/_template.md` — the template
- `docs/security-reviewers.md` — the roster
- `docs/agt-vendored-patch-audit.md` — vendored AgentMesh patch status
- `ci/security-audit-required.sh` — the enforcing script
