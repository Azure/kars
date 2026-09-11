# Contributing documentation

Bridge documentation should be usable by someone without access to session
history, private design notes, or a developer kubeconfig.

## Page types

- **Tutorial**: a complete learning journey.
- **How-to**: one operational task.
- **Concept**: architecture, boundaries, and rationale.
- **Reference**: exact configuration, APIs, roles, and compatibility.

## Rules

- State that `Azure/kars:kars-bridge` is an integration preview where availability
  matters; do not infer release readiness from source publication.
- State the required Kars version/commit for deployment instructions.
- Separate persona authorization from Kubernetes ServiceAccount RBAC.
- Distinguish live-qualified support from Helm template portability.
- Never link public readers to personal fork branches.
- Never place credentials, cookies, identity seeds, private keys, or customer
  resource names in examples.
- Add new pages to `docs/SUMMARY.md`.

## Validation

```bash
cd bridge
make check
(cd bff && cargo fmt --all -- --check)
helm lint deploy/helm/kars-bridge
helm template kars-bridge deploy/helm/kars-bridge \
  | kubectl apply --dry-run=client -f -
```

Any documented write path must also be tested through the deployed
`kars-bridge` ServiceAccount rather than only through a cluster-admin
kubeconfig.

## Permanent core/Bridge CI boundary

The integration candidate's core Rust, CLI and Kind jobs check out the repository
with `bridge/` physically absent. Core builds and runtime acceptance must not
acquire a mandatory dependency on the add-on.

Bridge native qualification emits its aggregate status for every PR targeting
the supported integration branches. Only root documentation-only changes can
skip native execution; CLI, runtime, mesh, chart, dependency, shipped-skill and
unknown source changes require it. Core-only Kind scope excludes Bridge-only
changes, which still require paired Bridge qualification. Pushes, manual runs
and reusable CI callers retain full qualification.

`Bridge component acceptance` aggregates every BFF, web, audit and add-on job
and runs even for core-only PRs. Failed, cancelled or skipped component jobs
cannot satisfy it. Together with `Require both native API and runtime acceptance`,
it provides stable check names for the integration merge policy rather than
relying on path-filtered jobs that may never report.

The aggregate rejects failed scope selection, missing outputs, and failed,
cancelled or unexpectedly skipped required jobs. An intentional documentation
skip is reported as not executed, never as runtime evidence. Existing real
add-on install/upgrade/uninstall checks retain their core resource/data
preservation assertions.

These workflow changes still require hosted qualification and integration into
the required merge-check policy. Complete supported-version and standing-Team
workflow coverage remains a separate acceptance requirement; passing scope or
template checks alone does not establish compatibility.
