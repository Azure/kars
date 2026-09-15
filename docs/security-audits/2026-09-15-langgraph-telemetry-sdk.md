<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# LangGraph TypeScript telemetry - bounded delegated source review

Source: `8c659f891e5bbde756fb9c7fd355e5699b9be9eb`.
Base: `4ec8c5a87d6efdb2262ad48b6cdfedcbea892988`.

Status: **Scoped source-approved; actual hosted execution remains required.**
This record does not approve deployment or waive any current-head check.

## Scope and evidence

The actual candidate image build, ACR run `chh0`, failed TypeScript compilation:
the source imported the removed `Resource` constructor and called the removed
`NodeTracerProvider.addSpanProcessor` method. The committed dependency manifest
and lockfile already select OpenTelemetry SDK 2.8.0.

The repair uses `resourceFromAttributes` and the tracer provider's constructor
`spanProcessors` option. Resource attributes, service-version fallback, endpoint
precedence, registration, metrics readers, best-effort failure handling and
already-initialized behavior remain unchanged. No dependency version, identity
scope, egress permission or telemetry destination is changed.

The Docker builder and production dependency stages now use the existing
committed lockfile through `npm ci`, rather than resolving floating dependencies.
The removed Vitest `basic` reporter was separately reproduced using the verified
4.1.8 runner; the normal default reporter replaces it.

Four regressions use actual SDK providers and a loopback HTTP collector. They
require real trace and metric exports, resource attributes, endpoint precedence
and preservation of the first initialization. A caught initialization error or
missing provider is a test failure, not a successful optional-telemetry result.
Both provider shutdown and collector cleanup are exercised.

The existing required `Runtime OpenClaw Build & Test` status retains its name
and existing steps, with explicit locked LangGraph install, typecheck, build and
test steps added. This closes the missing runtime-build coverage; it does not
replace or skip another required check.

## Review and qualification limits

Implementation was performed in the separate `h100-beta-image-build` AI context;
the parent integration context reviewed the complete six-file change and the
unchanged lockfile. This is bounded source review, not exhaustive assurance of
the entire telemetry SDK or a second human review.

The maintainer's publication-review delegation is recorded in
[comment 5615522306](https://github.com/Azure/kars/pull/551#issuecomment-5615522306).
The author attestation is exercised under that delegation, not a claim of
personal code review by the maintainer.

Local syntax, workflow/lockfile-preservation and whitespace checks passed.
The locked SDK 2.8.0 dependency graph is not available locally; cached 1.30.1 was
not substituted. Local build/typecheck and real exporter execution are therefore
**not claimed**. All 31 current branch requirements and actual hosted runtime/
container qualification remain mandatory before merge or H100 installation.
No old image may be relabeled as a build of the repaired source.

Signed-off-by: pallakatos (author source attestation through explicit maintainer-delegated AI review, not a claim of personal code review) <191481949+pallakatos@users.noreply.github.com>
Signed-off-by: GitHub Copilot (independent-context delegated AI source review, not a second human) <223556219+Copilot@users.noreply.github.com>
