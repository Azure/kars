# Contributing documentation

## Choose the page type

| Type | Purpose |
|---|---|
| Tutorial | A learning journey with a meaningful end result |
| How-to | A focused operational task |
| Concept | Architecture, rationale, boundaries, and trade-offs |
| Reference | Exact fields, commands, defaults, conditions, and compatibility |

Do not mix all four into one page.

## Writing standards

- Lead with the user outcome.
- State prerequisites and tested versions.
- Use copyable commands with expected results.
- Separate tested support from template portability and roadmap intent.
- Explain security boundaries and failure modes.
- Never include credentials, identity seeds, private endpoints, or personal-fork
  links in public documentation.
- Prefer Mermaid diagrams with text explanations.
- Link to generated API/reference sources rather than duplicating schemas.

## Validate locally

```bash
make docs-site
make docs-site-serve
```

For command examples, run the smallest relevant smoke test. For Helm examples:

```bash
helm lint deploy/helm/kars
helm template kars deploy/helm/kars -f deploy/helm/kars/values-local-dev.yaml >/tmp/kars.yaml
kubectl apply --dry-run=client -f /tmp/kars.yaml
```

## Review checklist

- Links and anchors resolve.
- Commands use current field names.
- Version and support claims match the compatibility matrix.
- Security claims specify the deployment mode.
- New pages are added to `docs/SUMMARY.md`.
- Generated files are changed through their generator, not by hand.
