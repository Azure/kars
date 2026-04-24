# Security Audit: `phase1/a2a-cli-list-exposed`

**Capability:** `azureclaw a2a list-exposed` and `azureclaw a2a schema`
CLI subcommands per ADR-0001 D6 sub-point 10.

## 1. Summary

- New `cli/src/commands/a2a.ts` with two subcommands.
- Wired into `cli/src/cli.ts`.
- Scaffold: no controller-side data source yet; returns the correct
  empty result for the current cluster state (no agents exposed).

## 2. Threat model delta

None — read-only CLI surface listing already-exposed agents. The
exposure decision happens in the controller (D6 sub-points 1–9);
this command surfaces it for human review.

A2A exposure is the highest-risk admin action in the platform; an
operator-friendly, scriptable inventory command lowers the chance
of an exposed agent being forgotten.

## 3. Tests

- 3 unit tests asserting command shape (3 pass).
- Total CLI test count unchanged otherwise.

## 4. Sign-off

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
