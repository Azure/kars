# `KarsEval` — Policy Conformance Runner

`KarsEval` is the **operator-facing surface** for replaying a signed
corpus of attack prompts (jailbreak, prompt-injection, banned-tool,
egress, memory-isolation) against a running `KarsSandbox` and stamping
a verifiable pass/fail verdict on the CR.

This page is the **operator guide** — what to run, what status to
look at, what each phase means. The schema lives in
[`crd-reference.md#karseval`](crd-reference.md#karseval--reproducible-evaluation-run).
For corpus authoring + signing, see
[`docs/cli-reference.md#kars-policy`](../cli-reference.md#kars-policy).

---

## What it does

```
operator   ──►   KarsEval CR   ──►   controller   ──►   Job / CronJob
                                          │                  │
                                          │                  ▼
                                          │            conformance-runner
                                          │              container
                                          │                  │
                                          │       hits sandbox router on :8443
                                          │       runs each case in the corpus
                                          │                  │
                                          ▼                  ▼
                                   status patch  ◄──   pod log: RunReport JSON
                                   (per-case verdicts +
                                    pass/fail counts)
```

The controller owns both the spawned `Job`/`CronJob` (via
`ownerReferences`, so they GC with the parent) and the materialised
corpus `ConfigMap`. The runner image is pinned globally via the Helm
chart (`KARS_CONFORMANCE_RUNNER_IMAGE`); per-CR override exists
for in-cluster dev only.

---

## Builtin corpora

Five corpora ship compiled into the controller binary and are
referenced by name via `spec.corpus.builtin`:

| Name | What it tests |
|---|---|
| `jailbreak-baseline` | Classic LLM jailbreak prompts (DAN, role-reversal, system-prompt extraction). Default if `spec.corpus` is omitted. |
| `prompt-injection-2026q1` | Indirect prompt injection via tool outputs, retrieved documents, and crafted user input. |
| `banned-tools` | Asserts the sandbox refuses calls to denylisted MCP tools (filesystem write outside `/sandbox/workspace`, raw shell, etc.). |
| `egress-known-bad` | Asserts the inference router blocks egress to known-bad hosts even when the agent is convinced to make the call. |
| `memory-isolation` | Asserts memory-store reads/writes can't cross sandbox boundaries. |

Source of truth: `eval-corpus/src/lib.rs::BUILTIN_NAMES`. Operators
can list them at runtime by reading the controller's `--help` or
sourcing a CR with `kubectl explain karseval.spec.corpus`.

For an external (signed) corpus, swap `builtin:` for `bundleRef:`
(`{ registry, repository, digest }`) — the controller verifies the
artifact's signature via the same path policies use; signing flow is
covered in the CLI reference under `kars policy sign`.

---

## Triggering a run

There are exactly two ways to start a run:

1. **Scheduled** — set `spec.schedule` to a 5-token cron expression.
   The reconciler ensures a `CronJob` owned by the eval. Editing
   `spec.schedule` updates the `CronJob.spec.schedule` in-place; no
   recreate.
2. **Run-now** — set the `kars.azure.com/run-now=true`
   annotation (or run `kars eval run <name>`, which sets the
   annotation for you). The reconciler ensures a one-shot `Job`,
   then clears the annotation so re-setting it triggers another
   run. Idempotent.

A CR with **both** a schedule and the run-now annotation will produce
both a `CronJob` and a one-shot `Job`. They run independently.

---

## Status — what to read

```
$ kubectl get karseval -A
NAMESPACE              NAME                SANDBOX     SCHEDULE      PHASE     LASTRUN   PASSED  FAILED  AGE
kars-my-agent     nightly-regression  my-agent    0 3 * * *     Ready     12h       41      1       3d
```

The printer columns (`Sandbox`, `Schedule`, `Phase`, `LastRun`,
`Passed`, `Failed`, `Age`) come straight off `status` and are the
fastest way to see "is my eval doing its job?".

### `status.phase`

Stamped by the reconciler from `(have-last-result, drifted)`:

| Phase | Meaning |
|---|---|
| `Pending` | CR has been admitted; no run has completed yet. Either the first run is in flight or `run-now` hasn't been set and there's no schedule. |
| `Ready` | At least one run has completed and the most recent one had `failed == 0`. |
| `Degraded` | The most recent run had `failed > 0` (drift). When `spec.failSandboxOnDrift=true` the target `KarsSandbox` is also patched to `Degraded` with reason `ConformanceDrift` via the `kars-controller/karseval-drift` field manager. |

### `status.conditions`

Three standard plus one KarsEval-specific:

| Type | When it goes `True` |
|---|---|
| `Ready` | Same trigger as phase=Ready. |
| `Progressing` | A run is in flight (Job exists and hasn't completed). |
| `Degraded` | Same trigger as phase=Degraded. |
| `ConformanceDrift` | Most recent run reported `failed > 0`. This is the operator's drift signal; it does not by itself patch the sandbox unless `failSandboxOnDrift=true`. |

Reasons used on each condition are listed in
[`docs/api/conditions.md#karseval`](conditions.md#karseval).

### `status.lastResult` and `status.history`

- `lastResult` carries the **full** summary of the most recent run:
  `schemaVersion`, `corpusLabel`, `corpusDigest`, `jobName`, and the
  pass/fail/errored counts. The reconciler reads the runner pod log,
  parses the `RunReport` JSON, and stamps it.
- `history` carries the last 20 (`MAX_HISTORY`) **summaries** in
  newest-first order. The reconciler trims older entries so the
  whole CR comfortably fits etcd's 1 MiB object cap. The 0-th entry
  always equals `lastResult`.

To diff the two most recent runs:

```bash
kars eval diff nightly-regression
```

For the per-case detail (which prompt failed and why), grab the runner
pod log directly — the reconciler keeps the spawning `Job.metadata.name`
in `status.lastResult.jobName`:

```bash
kubectl logs -n kars-system job/$(kubectl get karseval -n kars-system \
  nightly-regression -o jsonpath='{.status.lastResult.jobName}')
```

### Corpus digest drift

`status.corpusDigest` is the SHA-256 of the resolved corpus bytes
**as the controller saw them**. If a builtin corpus is updated by a
controller upgrade, or a signed bundle in the registry rotates to a
new digest, this field will change on the next reconcile. Combined
with `lastResult.corpusDigest` this lets operators answer
"did the corpus drift, or did the sandbox drift?" without leaving
`kubectl`.

The corpus is materialised into a `ConfigMap` (pointer in
`status.corpusConfigMapRef`) and mounted into the runner pod; the
runner re-hashes the bytes and refuses to start if the in-pod digest
disagrees with the CR-stamped one.

---

## CLI ergonomics

Four read-mostly subcommands:

```bash
kars eval list                  # tabular across the controller namespace
kars eval show <name>           # spec + last-run summary + drift status + conditions
kars eval run <name>            # set run-now annotation
kars eval diff <name>           # diff status.history[0] vs status.history[1]
```

All four hit the apiserver via `kubectl`; no router admin token
required. They work even when the router is unhealthy — useful for
finding out *why* a sandbox is Degraded.

Full reference: [`docs/cli-reference.md#kars-eval`](../cli-reference.md#kars-eval).

---

## Common workflows

### "Block CI on a known-good corpus"

Create one `KarsEval` per sandbox you want gated, with
`failSandboxOnDrift: true` and a low-frequency schedule (or run-now
in pre-merge CI). The sandbox flips to `Degraded` on the first failed
case; downstream callers see the condition and refuse to route new
sessions.

```yaml
apiVersion: kars.azure.com/v1alpha1
kind: KarsEval
metadata:
  name: ci-gate
  namespace: kars-my-agent
spec:
  targetSandboxRef:
    name: my-agent
  corpus:
    builtin: jailbreak-baseline
  failSandboxOnDrift: true
```

### "Run a custom corpus signed by my team"

```yaml
spec:
  corpus:
    bundleRef:
      registry: myacr.azurecr.io
      repository: eval-corpora/my-team-redteam
      digest: sha256:1f3a…
```

The controller verifies the OCI signature via the same path used for
ToolPolicies, refuses to materialise the `ConfigMap` if the signature
is missing/invalid, and stamps `Degraded` with reason
`SignatureVerificationFailed`. Sign with `kars policy sign --kind
eval-corpus` before pushing to the registry.

### "Smoke test a sandbox after a controller upgrade"

```bash
kars eval run nightly-regression   # one-shot
kubectl wait karseval/nightly-regression \
  -n kars-my-agent --for=condition=Ready --timeout=5m
```

---

## Garbage collection

The controller sets `ownerReferences` (`controller=true`,
`blockOwnerDeletion=true`) on every spawned `Job`, `CronJob`, and
the corpus `ConfigMap`. Deleting the `KarsEval` deletes all three.
Deleting the parent `KarsSandbox` does **not** cascade to `KarsEval`s
that reference it — those land in `phase=Pending` until the sandbox
is recreated or the `KarsEval` itself is deleted.

---

## See also

- [`docs/api/crd-reference.md#karseval`](crd-reference.md#karseval--reproducible-evaluation-run) — schema.
- [`docs/api/conditions.md`](conditions.md) — reason constants.
- [`docs/api/lifecycle.md`](lifecycle.md) — `Ready ⇔ router echo` invariant and how it applies to KarsEval (corpus digest match).
- [`docs/cli-reference.md#kars-eval`](../cli-reference.md#kars-eval) — CLI subcommand reference.
