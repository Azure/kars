# Branch Protection — `dev` and `main`

This is the canonical list of CI jobs that must be set as **required
status checks** on `dev` and `main` for AzureClaw. Setting these as
required is the source of truth for what blocks a merge.

## Required checks

The following `.github/workflows/ci.yml` jobs must pass on every PR
before merge:

| Job ID                  | Purpose                                                |
|-------------------------|--------------------------------------------------------|
| `rust-build`            | Workspace `cargo build --release` + `cargo test --all` |
| `cargo-deny`            | `cargo deny check` against `deny.toml`                 |
| `cli-build`             | TypeScript CLI: `npm run lint`, `npm run typecheck`, `npm test`, `npm audit --audit-level=high` |
| `runtime-openclaw-build`| Sandbox runtime build + `npm audit --audit-level=high` |
| `mesh-plugin-build`     | AgentMesh plugin build + `npm audit --audit-level=high` |
| `python-sidecar`        | Python sidecar tests                                   |
| `bicep-validate`        | Bicep template validation                              |
| `helm-lint`             | `helm lint deploy/helm/azureclaw`                      |
| `security-scan`         | Trivy filesystem scan                                  |
| `container-scan`        | Trivy image scan                                       |
| `dockerfile-lint`       | `hadolint` for every Dockerfile                        |
| `cosign-verify`         | Cosign keyless verification recipe (dry-run on PRs)    |

`cargo-audit` is **non-blocking** by design (`continue-on-error: true`
in `ci.yml`); advisories that surface mid-PR cannot block unrelated
work, but they are visible in the job log and triaged via `deny.toml`.

## CNCF AI Conformance

The `tests/cncf-conformance` crate runs as part of `rust-build` (it is
a workspace member). All 15 conformance criteria (C1–C15) are
enforced by `cargo test --all`; the test binary `cncf-conformance`
also writes `CONFORMANCE-REPORT.md` for human review.

If a criterion is intentionally relaxed (e.g., the `:latest`
image-tag convention documented in `docs/operations/supply-chain.md`),
update the criterion comment in `tests/cncf-conformance/src/lib.rs`
with a rationale that names the convention.

## Configuring branch protection

1. Repository **Settings → Branches → Branch protection rules**.
2. Add rule for `dev` and another for `main`.
3. Enable **Require status checks to pass before merging**.
4. Tick every job listed above.
5. Enable **Require branches to be up to date before merging**.
6. Enable **Require linear history** on `main`.
7. Enable **Require signed commits** on `main` (optional but
   recommended).

## When to add a new required check

Any new permanent CI row added under "supply-chain" or "conformance"
should be added to the table above and to the branch-protection
configuration in the same PR. Document the criterion in
`docs/operations/supply-chain.md` if it gates a release surface.
