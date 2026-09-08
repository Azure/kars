# Security Audit — Standing team control plane

Date: 2026-09-03
Scope: `controller/src/kars_team.rs`, `controller/src/kars_team_reconciler.rs`, `controller/src/team_commons.rs`, `controller/src/team_digest.rs`, `controller/src/kars_skill.rs`, `controller/src/kars_profile.rs`.
Gated paths: `controller/src/crd_validations.rs`.

## Summary

This slice adds declarative standing teams that materialize attenuated task
roles, generate cadence runs, preserve a provenance-indexed knowledge commons,
and expose operator health/digest state. Mesh delivery and runtime execution
remain outside this PR.

## T1: New capability / attack surface? (YES)

- Adds `KarsTeam`, `KarsSkill`, and `KarsProfile` APIs.
- Adds a cadence-driven controller that can create governed `KarsTask` runs.
- Adds controller-owned knowledge and digest ConfigMaps.

## T2: Security-control change? (YES)

- Team and role envelopes use the existing task attenuation lattice.
- Envelope authority fields are protected by a ValidatingAdmissionPolicy.
- Skills require a bounding policy and profiles/skills receive CEL shape
  validation.
- Knowledge entries retain source and run provenance.

## T3: Availability / fail-open risk? (REDUCED)

- Invalid rosters, profiles, skills, or capability dependencies degrade the
  team instead of launching partially governed work.
- Paused teams remain governed but idle.
- Run generation is idempotent and health state reports failed or stalled work.

## Verification

- Controller tests, full Rust workspace, clippy, formatting, Helm lint, CNCF
  conformance, LOC, and repository security gates.

## Verdict

Accept as a control-plane-only slice. Mesh delivery and runtime behavior are
intentionally deferred to the next stacked PR.

Signed-off-by: Pal Lakatos-Toth <pallakatos@github.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
