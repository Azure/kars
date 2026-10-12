<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Skills

Skills are versioned instructions and read-only reference files for selected
OpenClaw and Hermes missions and teams. They do not install plugins, execute
scripts, or grant additional shell, network, filesystem, or delegation authority.

## Lifecycle

1. A user submits a skill package.
2. Bridge stores the files in a same-namespace ConfigMap owned by the Skill UID.
   The controller validates the manifest, exact bytes, frontmatter, and bounding
   policy before publishing a content version.
3. A different operator opens **Review package** in Agent capabilities and
   inspects the recipe and every file.
4. Approval binds the displayed UID, generation, and content version. Concurrent
   changes reject the review instead of silently approving a newer package.
5. A mission or team selects the approved skill.
6. The controller rechecks approval and source bytes, then creates an immutable,
   digest-bound catalog in the owned sandbox namespace. The mission runtime
   exposes only those selected files through its existing read tool.

Changing the package invalidates approval; approval is not a mutable “trusted
name” flag.

Replacing a submitted version uses an identity-and-revision-conditional update,
not a forced ownership takeover. It removes omitted optional spec fields and
clears the old approval. A concurrent change is rejected for a fresh review.

The controller and Bridge use the same explicitly ordered content encoding,
independent of their JSON-library feature flags. A `Ready` phase alone does not
enable approval: Bridge also checks the observed generation and recomputes the
content version. If that binding differs, the review page reports the failed
check rather than repeating an obsolete controller validation message.

## Package shape

A package may include:

```text
SKILL.md
scripts/
references/
```

`SKILL.md` must begin with closed YAML frontmatter containing the matching
Skill `name` and a nonempty `description` of at most 1,024 UTF-8 bytes. A
recipe-only skill may omit files entirely.

Paths are relative ASCII names using letters, digits, `.`, `_`, `-`, and `/`.
Empty segments, `.`/`..` segments, `..` or `__` substrings, duplicate paths, and
file/directory conflicts are rejected. ConfigMap keys encode `/` as `__` and
must fit 253 bytes. Empty auxiliary files are preserved.

The canonical encoded file map and the combined runtime catalog each have a
1 MiB limit; catalog metadata counts toward that aggregate bound. Hermes reads
at most 65,536 bytes per file. Keep individual reference files within that limit
for cross-runtime use. Script files can be inspected as text, but selecting a
skill does not enable their execution. Never embed secrets.

## Separation of duties

Self-approval is always rejected. Submitter and reviewer subjects come from
authenticated sessions, not request payloads. User package replacement requires
the original submitter; operator raw-spec edits establish new authorship and
reset the review. Approval is manual content review, not malware scanning,
signature verification, or cryptographic attestation.

## Mission use

Currently approved skills appear in the mission and team composers. Mission
preflight checks current approval, source bytes, the exact bounding policy, and
explicitly selected required MCP servers. Skill names are part of the task's
authorization configuration. Source versions are resolved and pinned when the
runtime catalog is created; selection alone is not a receipt for a particular
source version.

The default `BRIDGE_ROUTE_QUALIFICATION_MODE=required` additionally requires
retained execution evidence for the selected route and exact Skill version.
An operator may explicitly configure `validation` on a test installation to
collect that first evidence. This mode permits unqualified runs of currently
bound MCP servers, memory stores, and independently approved Skills; it does
not mark them qualified or create evidence records. Missing current digests,
malformed evidence configuration, failed source approval, authorization,
budgets, tool policies, and runtime admission still block execution. External
channel evidence requirements are unchanged. Restore `required` for qualified
production admission.

Package use requires a reviewed filesystem-read phase with a positive tool-call
allowance. OpenClaw reads `/sandbox/.kars-skills/<skill>/<path>` with `file_read`;
Hermes reads `skills/<skill>/<path>` with `kars_mission_read`. These are virtual,
read-only paths: missing files never fall through to ambient disk. Normal input
and export restrictions remain unchanged. Files are not automatically exported
as mission deliverables.

For standalone reviewed plans, Bridge binds the initial run identifier in the
same conditional write that activates the Mission. Core can then admit the
plan without waiting for a sandbox that itself requires that run binding.
Existing revision identifiers are preserved.

## Updates and revocation

Changed, revoked, missing, or invalid sources block new runtime admission.
Selected running sandboxes are rechecked on the controller's reconciliation
cycle (normally 30 seconds); revocation is not instantaneous. A failed check
pauses only the owned workload. A changed catalog or removed selection requires
runtime recreation rather than replacing files under a running agent. Identical
content may resume after current independent approval is restored.

The shared Rust contract, typed CRDs, Helm schemas, BFF review endpoints, and
both runtime packages must ship together. Source tests or Ready pods do not
establish installed customer acceptance.

## Operator review checklist

- Purpose and expected outputs are clear.
- Every file has been inspected; instructions are appropriate for the task.
- Network and filesystem requirements are explicit.
- No secrets are embedded.
- Version digest matches the package under review.
- Upgrade and revocation behavior is understood.
