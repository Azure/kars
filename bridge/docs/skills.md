# Skills

Skills are versioned packages that extend an agent with instructions,
references, and executable helpers.

## Lifecycle

1. A user submits a skill package.
2. Bridge records the package bytes and version digest.
3. An operator reviews the exact version.
4. Approval binds the current generation and package digest.
5. A mission or team selects the approved skill.
6. Kars mirrors and mounts the verified bytes into the sandbox.

Changing the package invalidates approval; approval is not a mutable “trusted
name” flag.

## Package shape

A package may include:

```text
SKILL.md
scripts/
references/
```

Executable helpers must be complete, non-root, deterministic where possible,
and must not read ambient credentials.

## Separation of duties

Self-approval is rejected where the configured governance policy requires a
distinct approver. The actor comes from the authenticated session, not the
request payload.

## Mission use

Approved skills appear in the mission and team composers. Preflight binds the
selected skill generation into the launch-package fingerprint so a package
change cannot race the review.

## Operator review checklist

- Purpose and expected outputs are clear.
- Scripts are complete and have no placeholders.
- Network and filesystem requirements are explicit.
- No secrets are embedded.
- Version digest matches the package under review.
- Upgrade and revocation behavior is understood.
