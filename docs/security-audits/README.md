# Security audits

Lightweight, per-change security review records. The `security-audit-required`
CI gate (`ci/security-audit-required.sh`) requires any PR that touches a
**capability-introducing** path to add one
`docs/security-audits/<YYYY-MM-DD>-<slug>.md` here, signed off by two distinct
people (author + an independent reviewer).

Capability paths (see the gate for the exact regex): controller CRD/reconcilers/
admission, inference-router mcp/a2a/providers/routes, CLI commands/migrate/
adapters, OpenClaw runtime core, sandbox-image Dockerfiles/entrypoints, seccomp
profiles, and bundled Helm files. Test files are exempt.

## How to add one

1. Copy [`_template.md`](_template.md) to `YYYY-MM-DD-<slug>.md`.
2. Fill in the threat triage (T1 new surface? T2 control change? T3 availability?)
   and a short verdict.
3. End with two `Signed-off-by:` lines using real emails (author + reviewer).

Add a **new record for the current change scope**. Modifying or renaming an
older signed audit does not extend its approval to new capabilities. Identify
the reviewed source head and base, distinguish source review from executed
qualification, and retain unresolved findings and failed-run evidence honestly.
The gate rejects an unavailable review base rather than checking an unrelated
worktree diff.

The gate checks record presence and distinct signer emails; it does not
authenticate identities or verify that the prose covers the changed source.
Reviewers must check those facts. An explicitly maintainer-authorized delegation
must cite that authorization and disclose its actual participants and limits
in the record, never imply that AI review was a second human review or that
technical gates were waived. No delegation is inferred by default.

These docs are intentionally **tracked** (committed with the PR), unlike the
private `docs/internal/` planning folder.
