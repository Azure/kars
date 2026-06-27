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

These docs are intentionally **tracked** (committed with the PR), unlike the
private `docs/internal/` planning folder.
