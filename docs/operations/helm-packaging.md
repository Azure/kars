# Helm chart packaging

The Kars Helm chart lives under [`deploy/helm/kars/`](../../deploy/helm/kars). This page documents how the chart is **versioned** and how a maintainer **packages** it for a release.

> **Publishing the chart to a registry is a separate, deliberate step performed by a release manager with the appropriate credentials. This document covers only versioning and local packaging.**

## Versioning policy

The chart follows [SemVer](https://semver.org). Two fields in `Chart.yaml` track separate concerns:

| Field        | Meaning                                                                 | Bump on …                                              |
|--------------|-------------------------------------------------------------------------|--------------------------------------------------------|
| `version`    | Helm chart packaging version (templates, values schema, defaults).      | Any change to chart templates, values, or defaults.    |
| `appVersion` | The bundled Kars application version (controller + router image). | Whenever the controller or inference-router is rebuilt with a new release tag. |

Pre-release suffixes use SemVer dot-separated identifiers — e.g. `1.0.0-rc.1`, `1.0.0-rc.2`, `1.0.0` — never `1.0.0rc1` or `1.0.0_rc.1`.

The current version is recorded at the top of `deploy/helm/kars/Chart.yaml`.

## Packaging locally

Run the packaging script from the repository root:

```bash
make helm-package
# equivalent: bash deploy/helm/package.sh
```

The script will:

1. `helm lint` the chart and fail fast on any error.
2. `helm package` the chart into `./dist/charts/kars-<version>.tgz`.
3. Compute a SHA-256 alongside the tarball and write it to `<tarball>.sha256`.

The `dist/` directory is gitignored. The packaged tarball is **not** committed.

To package into a custom directory:

```bash
DEST=/tmp/charts bash deploy/helm/package.sh
```

## Cutting a release version

When preparing a release branch:

1. Update `version` and `appVersion` in `deploy/helm/kars/Chart.yaml`.
2. Update `CHANGELOG.md` with a section header for the new version.
3. Run `make helm-package` and commit only the metadata changes (not the tarball).
4. Open a PR to `dev`. The Helm Lint job in CI will re-validate the chart.

## Publishing (out of scope here)

Publishing the chart tarball to OCI (e.g. `oci://ghcr.io/azure/charts`) or a Helm HTTP repository is performed by a release manager outside this repo's CI. See the release runbook (internal) for that step.

## See also

- [`docs/operations/image-versioning.md`](image-versioning.md) — image tag policy that drives `appVersion`.
- [`CHANGELOG.md`](../../CHANGELOG.md) — release notes.
