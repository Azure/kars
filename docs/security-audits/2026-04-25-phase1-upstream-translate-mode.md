# Security Audit: `phase1/upstream-translate-mode`

**Capability:** adds `ClawSandbox.spec.upstreamCompatibility` schema-only
scaffold. Codifies §2 TranslateMode of the implementation plan as a
typed CRD field. Default OFF; no reconciler consumes the field yet.

## 1. Summary

- Adds `UpstreamCompatibilityConfig` to `controller/src/crd.rs`.
- Two fields:
  - `sigsAgentSandbox: Option<String>` — values `"off"`/`"observe"`/`"translate"`. Reconciler will refuse unknown strings (future branch).
  - `aiConformanceReference: bool` — emits canonical conformance status block when true. Schema-only.
- All defaults OFF; opt-in per-sandbox.

## 2. Threat model

Schema-only addition. No code path consumes the field. CRD validation
will refuse unknown `sigsAgentSandbox` values once the reconciler
lands; for now any value is accepted and ignored, which is harmless
because no code branches on it.

The `translate` mode itself, when implemented, will be **read-only
at the boundary** — AzureClaw never mutates upstream
`sigs.k8s.io/agent-sandbox` objects in-cluster. This audit pre-records
that invariant so the future reconciler branch can be reviewed against it.

## 3. Tests

- `cargo build --package azureclaw-controller` clean.
- `cargo test --package azureclaw-controller` — 125/125 pass.

## 4. Sign-off

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
