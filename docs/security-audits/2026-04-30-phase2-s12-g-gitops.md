# Phase 2 — S12.g: Sign-by-default + `--emit-manifest` GitOps mode

**Date:** 2026-04-30
**Slice:** S12.g (`phase2-s12-g-emit-manifest`) — S12 close-out
**Scope:** CLI-only change. Flips `--sign` to default-on inside a
signing context (`--enforce` / `--approve`); adds `--emit-manifest
<path>` for GitOps-mode promotion that writes a byte-stable
`ClawSandbox` patch to disk instead of running `kubectl patch`. **No**
controller, router, or sandbox-image change.

## Existing implementation surveyed

- `cli/src/commands/egress.ts` — pre-S12.g `--sign` family flags +
  guard `--sign requires --enforce or --approve` (added in S12.c).
- `cli/src/commands/egress/sign.ts` — canonical YAML builder + oras
  push + cosign sign + kubectl patch helpers (S12.c).
- `cli/src/commands/migrate.ts` — `from-kagent` runner; emits
  `ClawSandbox` resources with optional `spec.networkPolicy.allowedEndpoints`.
- `controller/src/policy_fetcher.rs` — authoritative-mode verifier
  (S12.b/d/e); emits `AllowlistVerified=False/SignerPolicyMissing` for
  unsigned artifacts when a `SignerPolicy` is installed.
- `docs/policy-canonical-format.md` — v1 canonical bytes spec, byte-stable
  hand-rolled emitter (S12.a).

## Trust model summary

S12.g changes the **default policy** that ships with the CLI. It does
not change the on-cluster trust model:

- The cluster trusts cosign-signed canonical egress allowlist
  artifacts whose Fulcio identity matches the cluster `SignerPolicy`.
- An unsigned artifact is rejected by the controller in authoritative
  mode (`AllowlistVerified=False/SignerPolicyMissing`), regardless of
  how it got onto the CR.
- The CLI is one possible producer; CI pipelines using GitHub Actions
  OIDC are another. Both produce identical canonical bytes. The
  emit-manifest output is also byte-stable so the producer can be
  audited from the GitOps repo (digest + signer identity in the
  comment header).

## Threat surfaces introduced

This slice changes one default and adds one new I/O surface (writing
a YAML file to operator-supplied path). It does NOT add new
subprocess invocations, OIDC flows, or network endpoints.

### T1 — Default-on signing creates artifacts the operator did not realize they were producing

**Threat.** An operator running `azureclaw egress my-agent --enforce`
who is unaware of the default flip publishes a signed artifact to ACR
unintentionally. If the operator's machine is in keyless mode, this
binds their personal Fulcio identity to the artifact and writes a
public Rekor entry.

**Mitigation.** The CLI prints the registry, repository, generation,
endpoint count, and sign mode before pushing — same banner that
shipped in S12.c, unchanged. Operators see exactly what is about to
be signed under their identity. Opt-out is one flag: `--no-sign`.

**Residual risk.** Low. Operators reading the CHANGELOG see the
default-flip explicitly called out under "BREAKING". The pre-push
banner is identical to the S12.c banner, so muscle memory transfers.

### T2 — `--no-sign` produces a CR that the controller silently refuses in authoritative mode

**Threat.** An operator opts out with `--no-sign`, expecting the
allowlist to be applied. In authoritative mode (S12.e) the controller
emits `AllowlistVerified=False/SignerPolicyMissing` and either
falls back to the LKG cache or fails closed (refuses the sandbox).
The operator sees the CLI report success while the cluster has
silently refused.

**Mitigation.** The CLI prints a yellow `⚠ --no-sign:` warning that
explicitly names the controller condition (`AllowlistVerified=False/
SignerPolicyMissing`) and the resulting behavior ("refuse the
artifact in authoritative mode"). The warning is impossible to miss
on a TTY. CI pipelines should review their stderr.

### T3 — `--emit-manifest` overwrites an unrelated file

**Threat.** An operator (or a CI run) passes a path that resolves to
an existing file with unrelated contents and clobbers it.

**Mitigation.** The CLI refuses to write when the target file exists
unless `--force` is also set. CI pipelines use `--force` deliberately
because the path is committed to the GitOps repo and re-runs are
expected. The error message names the path and the `--force`
escape hatch:
`refusing to overwrite existing file <path> (pass --force to override)`.

### T4 — `--emit-manifest` is combined with `--no-sign`

**Threat.** GitOps mode promotes the artifact off-cluster — the
operator is no longer present at the moment the cluster verifies it.
An unsigned artifact promoted via GitOps would fail authoritative-mode
verify with no human in the loop to retry.

**Mitigation.** The CLI errors loudly:
`--emit-manifest cannot be combined with --no-sign — GitOps mode
requires signed artifacts.` There is no escape hatch. Fail-fast
before any I/O.

### T5 — Tampering between emit and apply

**Threat.** A malicious actor with write access to the GitOps repo
edits the digest in the emitted YAML to point at a different
artifact (e.g., one signed by a compromised identity from a previous
release).

**Mitigation.** Out of scope for the CLI — this is a GitOps repo
trust problem, addressed by signed commits, branch protection, and
required reviewers on the GitOps repo. The CLI cooperates by:
- Putting the digest + signer identity in the leading **comment**
  (immutable from the controller's view but human-visible during PR
  review).
- Marking the resource with
  `metadata.annotations["azureclaw.io/applied-via-gitops"]="true"` so
  cluster-side audit can distinguish GitOps-applied allowlists from
  `kubectl patch`-applied ones (downstream tools can require a
  matching commit signature).
- The on-cluster controller still verifies the digest + cosign signature
  (S12.b/d/e) — a tampered digest pointing at an artifact whose
  signer is not in the `SignerPolicy` is refused regardless of how it
  reached the CR.

## Implementation sites

- `cli/src/commands/egress.ts`
  - New flags: `--emit-manifest <path>`, `--force`.
  - Guard logic: `--emit-manifest` requires `--enforce`/`--approve`;
    `--emit-manifest` + `--no-sign` errors; default-on signing inside
    signing context; loud `⚠` warning for `--no-sign` opt-out.
  - `runSignFlow` branches on `options.emitManifest` after signing —
    skips `kubectl patch` and writes the manifest YAML instead.
- `cli/src/commands/egress/sign.ts`
  - New helpers: `buildEmitManifestYaml`, `writeEmitManifest`,
    `describeSignerIdentity`.
  - `buildEmitManifestYaml` is hand-rolled (no `yaml` lib) for byte
    stability across CLI versions / Node minor releases.
- `cli/src/commands/migrate.ts`
  - `from-kagent` runner emits a "Next step (S12.g)" hint to stderr
    when the translated bundle includes an egress allowlist.
- `cli/src/commands/egress.test.ts`, `cli/src/commands/egress/sign.test.ts`
  - +17 unit tests covering: emit-manifest flag wiring, `--no-sign`
    incompatibility, `--enforce`-required guard, byte stability,
    overwrite-protection / `--force`, signer-identity descriptor.
- `docs/operations/gitops.md` — operator walkthrough, workflow diagram,
  CI snippet, failure-mode table.
- `docs/policy-canonical-format.md` — Producer section updated to call
  out sign-by-default + emit-manifest flow.

## Test delta

| Suite                                | Before | After | Δ    |
| ------------------------------------ | -----: | ----: | ---: |
| `cli/src/commands/egress.test.ts`    |      8 |    11 |  +3  |
| `cli/src/commands/egress/sign.test.ts` |    33 |    47 |  +14 |
| **CLI total (vitest)**               |    434 |   451 |  +17 |

Rust suites unchanged (this is a CLI-only slice).

## What's intentionally out of scope

- No new controller-side validation for the `azureclaw.io/applied-via-gitops`
  annotation (treated as informational; cluster-side audit hooks can
  consume it independently).
- No automatic git commit / PR creation — the CLI writes the file and
  prints a "Commit this file" hint; integrating with `gh pr create`
  or similar is left to the operator's CI/CD recipe.
- No bulk emit (one ClawSandbox per invocation). Multi-sandbox
  emission is a backlog item — keep S12.g tight to the single-sandbox
  flow.
