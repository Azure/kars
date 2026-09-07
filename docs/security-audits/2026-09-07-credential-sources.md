# Agent credential-source capability review — 2026-09-07

**Status:** additive candidate; independent security review and human sign-off
pending. Automated tests are evidence, not approval. No reviewer identity,
organizational approval, compliance certification, or production qualification
is asserted by this document.

## Scope and base

Base: namespace prerequisite `62093414cb8d5d9937c1d9974504047c84669d6c`
(Azure/kars#548). This slice adds an optional UID-pinned KarsSandbox credential
source, controller-owned projection, CLI source lifecycle, and schema/docs.
It does not change Bridge permissions, inference credentials, signing providers,
agent transport, or customer/live deployments.

The optional field is omitted from serialization when unset, preserving old
Sandbox-spec inputs. When present, its name/UID remain in serialized spec
digests. Existing Task/Team blueprint and receipt formats are not extended:
mutable credential values are not claimed to be covered by a governance receipt.

## Authority boundaries

| Boundary | Control |
|---|---|
| Arbitrary Secret read oracle | Fixed source-name derivation; same CR workspace; exact UID; explicit purpose/intent/target before reading values |
| Source type and data | Opaque only; existing agent channel/search allowlist; provider/control-plane/process keys rejected |
| Sandbox recreation | Exact source owner binding to Sandbox UID; no automatic name-only future delivery |
| Workspace/name collisions | Namespace claim v1 and runtime namespace UID rechecks |
| Foreign projection | Exact projection purpose, namespace owner reference, Sandbox/workspace/namespace UID bindings |
| Cross-resource write races | Empty anchor, source/Sandbox/namespace rechecks, UID/RV-fenced final value patch; no force takeover |
| Rotation/removal | Exclusive collection, explicit removed-key nulls, UID/RV revision, controlled stop/Recreate |
| Source invalidation | Stop verified runtime, revoke only owned projection, report failure, bounded retry; no legacy fallback |
| Error disclosure | Kubernetes API errors reduced to stage/status; no request bodies or credential values in source errors |
| CLI authority | Source before CR CREATE; exact returned UID; source update CAS; explicit UID reselection; schema-retention and controller-version acknowledgement checks |

Unconfigured Sandboxes keep their direct EnvFrom collection. Explicit opt-in
performs a one-time CLI migration without editing the direct Secret. Explicit
opt-out restores that direct collection. Neither operation may silently move
foreign or provider/control-plane data through the source primitive.

## Automated evidence

Existing Rust/Vitest/Helm runners cover:

* Missing-reference legacy behavior and default schema/serialization.
* Explicit source creation/binding and fenced projection writes.
* Key addition, update, removal, empty collections, and metadata-only changes.
* Missing/replaced source, wrong workspace/name, foreign owner/type/purpose,
  namespace replacement, destination collision, and API/CAS races.
* Owned-only revocation and controller deployment pause/refresh.
* Source values excluded from error text and process-environment injection.
* CLI pre-CR storage, actual returned UID binding, collection migration,
  update/removal/disable, safe errors, and direct-path compatibility.
* Generated/reference schema parity and Helm admission-rule rendering.

Candidate qualification on 2026-09-07:

* 66 targeted controller tests passed (credential-source, namespace ownership,
  and SRE-writer compatibility selectors), offline/locked with incremental
  compilation disabled.
* Controller all-targets Clippy passed with warnings denied.
* 90 targeted CLI/Vitest tests passed, including Helm rendering and new-module
  size/header/no-stub/no-custom-crypto assertions.
* TypeScript typecheck and changed-file oxlint passed.

The existing disposable Kind harness now includes a source-bound BYO fixture
using its already-loaded sandbox test image. The consumer emits fixed markers,
not environment values. The lifecycle covers actual process environment on
initial delivery, rotation and key removal; missing-source revocation with no
legacy fallback; explicit opt-out restoring the preserved direct collection;
and cleanup preserving the core namespace. Shell syntax is checked locally;
execution of this new case awaits hosted CI.

Hosted Kind execution at `b69a6ad6` exposed a real installation blocker:
the new CEL rule referenced `upstreamCompatibility`, which was missing from the
handwritten Helm schema. Kubernetes rejected the entire Sandbox CRD, so no
credential-consumer lifecycle result was established. Local rendering and
source review were insufficient to catch that API-server compilation failure.

The repair declares the existing Rust compatibility fields in the optional
Helm schema, retains the source/overlay admission guard, and requires an upstream
reference for overlay mode. The targeted schema assertion now checks the
referenced field definitions, not just the presence of rule text. The Kind
fixture also requires the intended overlay rejection message, and Helm setup
failure now stops the harness instead of producing cascading secondary failures.
Fifteen targeted schema/Helm tests, typecheck, scoped lint, Helm lint and shell
syntax pass locally; the repaired head still requires real API-server execution.

Diff-based publication gates still run on the parent's eventual atomic commit;
the candidate was intentionally not committed or pushed here. No live
Kubernetes admission or customer rollout was executed. Real-cluster
qualification and independent/human review remain release gates.

## Remaining limits and required review

1. Kubernetes source/target operations are not transactional. Last-read identity
   checks plus a fenced target write prevent blind replacement writes; watches
   and a bounded retry provide eventual source revocation.
2. API failures/node partitions can delay process termination. External
   credential invalidation and secrets already observed by an agent are outside
   this feature's guarantees.
3. Namespace administrators remain trusted. Namespace claim annotations are not
   cryptographic attestations or protection from forced namespace finalization.
4. CLI credential flags retain their existing shell/process-argument exposure.
   Kubernetes source payloads themselves travel through stdin and are not logged.
5. Bridge still needs a separate reviewed adapter and least-privilege/admission
   changes. This candidate must not be described as Bridge Secret-RBAC closure.
6. Review schema/controller mixed-version handling, consumer rollout behavior,
   and source-authoring permissions before release.

**Human sign-off: pending. Independent security sign-off: pending.**
