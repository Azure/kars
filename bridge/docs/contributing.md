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
