# Capability audit - Evaluator runner compatibility

Date: 2026-09-10
Status: **Bounded source approval under explicit maintainer delegation**.
Current-base hosted/native qualification remains required before publication.

## Scope and authorization

Reviewed source: `64c4c717e6af62bb35fdf4a204cea7590e3676c8` against actual
integration base `af4deba75739acceb5f8c1b605312ce7d5ad444f`.

The maintainer authorized publication sign-offs after focused review rounds in
[comment 5615522306](https://github.com/Azure/kars/pull/551#issuecomment-5615522306).
The attestation below identifies Copilot as delegated AI review, not a second
human reviewer. No technical gate, identity, approval or native result is
fabricated or waived.

This is an existing-product compatibility repair, not a new evaluation
capability or general evidence-store implementation. The canonical product
already omits the unsupported argument and declares restricted Pod settings.
The public controller still supplied that argument and omitted those settings.

## T1: New capability or attack surface? (NO)

The existing Job/CronJob runner Pod builder is extracted into a small module.
The runner command no longer passes `--corpus-label`, which its actual packaged
Clap parser rejects. Source labels remain in corpus ConfigMap annotations and
controller-owned evaluation results; the report wire format is unchanged.

Both existing scheduling paths call the same builder. No new endpoint,
permission, credential, dependency, resource kind or image is introduced.
Contract tests import the actual producer and actual runner parser, not a
duplicated argument schema.

## T2: Security-control change? (YES, restrictive)

Pods and containers explicitly require non-root execution and RuntimeDefault
seccomp; containers disallow privilege escalation and drop all capabilities.
These are the restricted Pod Security Standard settings already used by the
canonical product. Namespace admission is not weakened or bypassed.

The builder does not force a particular numeric UID onto custom runner images.
It preserves their declared numeric non-root user; the shipped image already
declares USER 1000:1000. This compatibility choice avoids unnecessarily changing
custom non-root image filesystem access. Root-running images are not authorized.
Existing image selection, resource bounds, corpus mounts and report destination
are otherwise unchanged.

## T3: Availability or fail-open risk?

The repair removes a deterministic launch failure and supplies required Pod
settings rather than relaxing validation. Unsupported CLI arguments continue
to be rejected by the runner. Missing or malformed evaluation results do not
become successful merely because this launch contract is repaired.

This review does not approve unrelated pre-existing evaluator ownership/SSA
behavior or claim complete report-retention, transport-error classification or
receipt-log parity. Those remain separately scoped publication work.

## Verification and review

Before the fix, the actual producer-to-parser regression failed with
`UnknownArgument` for `--corpus-label`. After the fix:

- 47 runner unit cases passed.
- Seven contract/module cases passed, including four existing parser cases
  compiled into that test module; these are not seven distinct new regressions.
- Five existing end-to-end runner cases passed.
- 31 controller-binary evaluation cases passed.
- Strict controller/runner all-target Clippy, formatting, whitespace and the
  committed LOC gate passed.

An independent focused AI compatibility review of the exact source comparison
reported no significant issues. It covered both scheduling paths, source label
retention, custom non-root image compatibility and the actual contract tests;
it did not independently execute a native cluster.

All successful Cargo qualification used the existing shared target,
offline/locked dependencies, two jobs and incremental compilation disabled.
The successful batch minimum was 8.60 GiB above the unchanged 8.5 GiB guard.
An earlier disk-floor interruption was not counted as a passing test or as the
argument reproduction.

Native Pod admission and final current-base publication qualification are
still required. No public push, deployment, H100 mutation, new dependency
installation, source from private SDK/probe ancestry or main promotion is
authorized by this source approval.

Signed-off-by: pallakatos (maintainer delegation recorded above) <lakatos.toth.pal@gmail.com>
Signed-off-by: GitHub Copilot (delegated AI review, not an independent human) <223556219+Copilot@users.noreply.github.com>
