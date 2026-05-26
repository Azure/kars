# Security Audit: PR A — Critical Hygiene Batch (pre-OSS)

**Capability:** Address Critical findings from the 2026-05-15 OSS-readiness
auditor report (361-line summary, archived in
`session-state/.../files/paste-1778877697377.txt`).

## 1. Summary

PR A lands 7 of 9 Critical findings (C1, C2, C3, C6, C7, C8, C9) as a
single low-risk batch. C4 (ACR OIDC swap) and C5 (Bicep
`diagnosticSettings`) are explicitly deferred because both require
Azure-side infra provisioning (federated credentials / Log Analytics
workspace pre-grants) that is out of band with this code change. They
will land separately once the provisioning is staged.

The non-deferred Critical fixes are all small, mechanical, and
preserve wire / on-disk compatibility. Two are pure documentation
honesty rewrites (C3, C8), one is a comment-only honesty fix (C2),
three are mechanical (C1 CSPRNG swap, C6 Dockerfile HEALTHCHECK, C7
SemVer reconciliation across `Cargo.toml` / `package.json` /
`Chart.yaml` / README), and one is a docs cleanup of broken
`docs/internal/*` pointers from public-facing pages (C9).

A bonus item — fixing 11 broken Mermaid blocks across 5 documentation
files — is bundled because it surfaced naturally while doing the C9
pass. All 46 Mermaid blocks in the docs tree now validate against
`mmdc` v11.

## 2. Scope

### 2.1 C1 — CSPRNG for bootstrap tokens
`controller/src/reconciler/mod.rs` gateway-token (~L748) and
admin-token (~L807) generation. Was: `RandomState` + `SystemTime::now()`
nanoseconds (predictable). Now: inline `use rand::RngCore;` then
`rand::rng().fill_bytes(&mut buf)` and hex-encode. Wire format
preserved — gateway-token stays 32-char hex, admin-token stays
64-char hex — so existing in-cluster Secrets keep working. Lazy
migration on next reconcile.

### 2.2 C2 — Honest comment on `0.0.0.0` bind
`inference-router/src/main.rs:405`. Replaced misleading "container
network is isolated by NetworkPolicy" comment with a multi-line
comment explaining that the bind is required because the controller's
`router_confirmation` module reaches the router cross-pod via Service
DNS, and listing the three defense-in-depth layers (NetworkPolicy,
admission webhook ban on pod-exec into router container, bearer-token
auth on admin/egress/sandbox/AGT routes).

### 2.3 C3 — Honest envelope description in mesh-plugin identity
`mesh-plugin/src/identity.ts`. Top docstring previously claimed the
envelope was "encrypted at rest with a host-derived key". Rewrote to
describe it as obfuscation only: the KEK is derived from host-readable
inputs (hostname, UID, container path), so an attacker with on-host
read access can re-derive it. On-disk format unchanged for back-compat
with existing operators. Inline comment added on `deriveEncryptionKey()`.

### 2.4 C6 — Dockerfile HEALTHCHECKs
- `inference-router/Dockerfile`: added `curl` to the tdnf install in
  the runtime stage; HEALTHCHECK CMD probes `/healthz` on `:8443`.
- `sandbox-images/openclaw/Dockerfile`: HEALTHCHECK probes the gateway
  on `:18789` (curl already present in base image).

### 2.5 C7 — Version reconciliation
README.md / `cli/package.json` / `cli/package-lock.json` /
`deploy/helm/kars/Chart.yaml` were in three-way drift
(`v1.0.0-rc.1`, `0.1.0-alpha.1`, `1.0.0-rc.1`). All aligned to
`0.1.0`; README phrased "v0.1.0 — pre-1.0 development" so the
pre-release status is plain. CHANGELOG.md historical entries and
docs/operations/helm-packaging.md illustrative example (which
references `rc.1` as a SemVer example) left intact — they're
historical / example use, not active claims.

### 2.6 C8 — Drop vendored-overlay references from public docs
README.md and `docs/architecture.md` previously described
`vendor/agentmesh-sdk/dist/` as overlaid into the sandbox image. That
overlay was retired in Phase 5.2; the sandbox now installs the
upstream `@agentmesh/sdk` via npm without an overlay. Both
descriptions rewritten to match reality. The `vendor/` directory and
its patch READMEs remain in-repo for documentation / regression
reference but are not consumed by builds.

### 2.7 C9 — Remove dangling `docs/internal/` pointers from public docs
Thirteen user-facing docs had hyperlinks or trailing
"see also: `docs/internal/...`" lines that pointed into the
gitignored `docs/internal/` tree. Strategy adopted after operator
review: avoid the `(internal)` annotation pattern (reads as
tease-y / gatekeeping in OSS context). Either inlined a prose summary
of the referenced content (where it was load-bearing, e.g.
lifecycle.md Compiled-state explanation) or simply removed the
pointer (where it was a "see also" list line). The two remaining
references to `docs/internal/` in `docs/README.md` and
`docs/site/README.md` are self-aware (they exist to explain what the
`docs/internal/` directory is) and were left intact.

### 2.8 Bonus — Mermaid block fixes
Eleven Mermaid blocks across `docs/use-cases.md`,
`docs/api/lifecycle.md`, `docs/architecture/a2a-gateway.md`,
`docs/agt-vs-vendored-sdk.md`, and `docs/architecture-diagrams.md`
failed parsing in `mmdc` v11. Root causes: `\n` escape sequences
inside flowchart node labels (should be `<br/>`), HTML entities
`&lt;`/`&gt;` inside sequenceDiagram message text, unquoted parens in
node labels, unquoted package names beginning with `@`, and
semicolons in sequenceDiagram message text. All 46 blocks in the docs
tree now validate. Validator harness retained at
`/tmp/mermaid-check.mjs` for the duration of the session.

## 3. Explicitly out of scope

- **C4 (ACR OIDC swap)** — requires Azure-side federated credential
  provisioning. Attempted in-band and reverted on operator direction.
  Both the old `ACR_USERNAME/PASSWORD` and a new `AZURE_CLIENT_ID`
  OIDC path are currently unprovisioned secrets in the GitHub repo,
  so an in-band swap would be cosmetic. Tracked as a follow-up: stage
  the federated credential first, then swap the workflow.
- **C5 (Bicep `diagnosticSettings`)** — needs a Log Analytics
  workspace target. Deferred per operator direction.

## 4. Threat-model deltas

- **C1** materially raises the bar against predictability attacks on
  the bootstrap admin/gateway tokens. The previous construction was
  `hash(nanos_of_systemtime + RandomState_seed)`; the new construction
  is OS CSPRNG. No threat-model change beyond strength.
- **C2** is comment-only; no behavior change. The bind is
  `0.0.0.0:8443` before and after. The three defense layers in front
  of it (NetworkPolicy egress/ingress rules on the sandbox namespace,
  admission webhook denying `pod/exec` into the router container,
  and bearer-token auth on every admin/egress/sandbox/AGT route) are
  unchanged.
- **C3** is documentation-only; on-disk format unchanged. The
  envelope was always obfuscation, never encryption — we now say so.
- **C6** adds liveness signalling; no new network exposure
  (HEALTHCHECK is a local container probe).
- **C7 / C8 / C9 / Mermaid** are documentation-only.

## 5. Secrets handling

No new secret material introduced. C1 changes the entropy source for
existing token Secrets; format is preserved so existing tokens keep
working until next rotation.

## 6. Test coverage

- Rust workspace `cargo build --release` clean
  (controller + inference-router release builds).
- `cargo clippy --all-targets -- -D warnings` clean.
- `cargo fmt --all` clean.
- CLI: `npm run build`, `npm run typecheck`, `npm test`
  (769 passed / 2 skipped / 771 total), `npm run lint` clean.
- All 46 Mermaid blocks in `docs/` + `README.md` validate against
  `@mermaid-js/mermaid-cli@11.15.0`.

No new tests added — the changed surfaces (token generation, bind
comment, dockerfile HEALTHCHECK, doc text) are not unit-testable in
a meaningful way beyond what their build/lint gates already provide.

## 7. Sign-offs

Signed-off-by: Pal Lakatos <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
