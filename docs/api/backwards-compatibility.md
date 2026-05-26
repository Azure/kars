# Backwards-Compatibility Commitment

## Versioning model

AzureClaw follows [Semantic Versioning 2.0.0](https://semver.org/) on the **public CLI + CRD surface**. Internal Rust crate versions track the workspace version but are not a stable Cargo dependency.

| Surface | Stable from | Compatibility window |
|---|---|---|
| `ClawSandbox` CRD | `v1.0.0` (schema `apiextensions.k8s.io/v1alpha1`) | `v1alpha1` is **frozen** for the entire `1.x` line — see [`architecture/crd-versioning.md`](../architecture/crd-versioning.md) |
| `azureclaw` CLI commands & flags | `v1.0.0` | Two minor versions of deprecation before removal |
| Helm chart values keys | `v1.0.0` | Two minor versions of deprecation before removal |
| Inference router HTTP routes (north + east-west) | `v1.0.0` | Major bump for breaking change |
| Inference router internal sandbox seam (`127.0.0.1:8443`) | Internal | May change in any release |
| Provider trait shapes (`MeshProvider`, `PolicyDecisionProvider`, `AuditSink`, `SigningProvider`) | Internal | May change in any release |
| Vendored AgentMesh on-the-wire format | Internal | Locked to vendored relay/registry version; bumps require re-roll |

## What "stable" means

For each stable surface:

- **No silent breaking change.** Removal of a flag, value key, route, or CRD field requires:
  1. A `[DEPRECATED]` notice in `CHANGELOG.md` and the relevant doc.
  2. Two minor releases where the old surface keeps working (with a runtime warning).
  3. Removal in the next major release, called out in the release-notes "Breaking changes" section.
- **Additive change is allowed in minor releases.** New CLI subcommands, new CRD optional fields, new Helm values keys, new router routes.
- **Bug-fix changes** in patch releases preserve the contract — even if the old behaviour was wrong by the spec, we keep it bug-compatible until the next minor unless the old behaviour is a security flaw.

## Security exceptions

The compatibility window is **explicitly waived** when the change closes:

- A confirmed CVE in AzureClaw or any directly vendored component.
- A privilege-escalation path inside the sandbox boundary.
- A token / key disclosure path.

Such changes ship in a patch release with a `SECURITY` line in the release notes.

## CRD migration policy

See [`architecture/crd-versioning.md`](../architecture/crd-versioning.md) for the full v1 policy. Highlights:

- `v1alpha1` is the only served version for the entire `1.x` line.
- `v1alpha2` lands in a v1.1+ release alongside a conversion webhook that converts both directions.
- We never delete fields from `v1alpha1`; deprecated fields become no-ops with a controller log line.

## CLI migration policy

When a flag is renamed:

- The old flag continues to work and prints a `[DEPRECATED]` line on stderr.
- The flag stays for ≥ 2 minor versions.
- Help text shows both forms during the deprecation window.

When a subcommand is renamed:

- The old subcommand becomes a thin alias and prints a `[DEPRECATED]` line.
- Same ≥ 2 minor-version window.

## Helm values migration policy

When a values key is renamed:

- `values.yaml` documents both forms during the deprecation window.
- The chart `_helpers.tpl` uses `coalesce` to honour both keys.
- The chart `NOTES.txt` prints a deprecation warning when the old key is set.

## Release-notes template

Every release includes the standard sections (see [`r5-changelog`](../../CHANGELOG.md) for the canonical list):

- Highlights
- Added (additive surface changes)
- Changed (compatible behaviour changes)
- Fixed (bug fixes)
- **Deprecated** (with target removal version)
- **Breaking changes** (only in major releases) — empty for `1.x.y` releases
- Security
- Internal (refactors, test churn — informational only)

## Forward-looking notes

- A CRD `v1alpha2` is on the roadmap (additive only) alongside a conversion webhook so existing `v1alpha1` clients keep working without change. See [`docs/roadmap.md`](../roadmap.md) for the current target window.
- The provider-trait surface is internal and *will* change between minor versions. External consumers should integrate at the CRD or CLI level, not the trait level.
