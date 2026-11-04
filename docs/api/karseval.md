<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# `KarsEval` — Policy Conformance Runner

`KarsEval` is the **operator-facing surface** for replaying a signed
corpus of attack prompts (jailbreak, prompt-injection, banned-tool,
egress, memory-isolation) against a running `KarsSandbox` and stamping
a verifiable pass/fail verdict on the CR.

Bridge's new-eval selector excludes delivered Mission sandboxes even when their
retained phase is Running or Ready. Active Missions and standing sandboxes such
as SRE remain selectable. If live refresh removes the selected target, choose
another available target before submitting. This inventory filter is not a
router-health guarantee; transport failures remain inconclusive eval results.

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
chart (`evals.runnerImage`, exposed as `KARS_CONFORMANCE_RUNNER_IMAGE`);
per-CR override exists for in-cluster dev only. The Kind installer builds and
loads its local runner rather than requiring a public image pull.

After funded SRE ownership adoption, changing the Core runner requires
`evals.runnerImageBaseline` containing the exact runner reference from the
original adoption, alongside an immutable `evals.runnerImage` in
`repository:latest@sha256:<digest>` form. A local qualified mirror may replace
the public default. The baseline must reproduce the sealed adoption digest;
it is not permission to change other values, SRE policy, funding or receipts.
Retain that original baseline on subsequent upgrades. This transition cannot
be combined with initial adoption.

Scheduled and run-now Jobs use the same runner Pod contract. The command
passes the mounted corpus path, router URL and output path accepted by the
packaged runner. The corpus source label remains in ConfigMap annotations
and evaluation status; it is not an additional runner CLI option.

For `MemoryRead`, the controller supplies `--memory-store` from the target's
explicit `memoryRef`. The binding must be Ready at its current generation, with
matching compiled/loaded digests and the exact published binding ConfigMap.
The memory resource identity, content, and ConfigMap identity are included in
workload intent and revalidated before result adoption. Missing, changed, or
unavailable bindings are evaluation errors, not successful isolation evidence.
The runner uses the existing governed REST `search_memories` route, preserving
the requested scope exactly; it does not bypass loopback-only platform MCP or
receive additional Kubernetes permissions. Standalone memory probes require an
explicit valid `--memory-store`. Wrong-store, upstream-auth, and generic HTTP
403 responses do not satisfy the scope-denial corpus.

Generated names preserve existing short names. Longer names retain a readable
prefix and a hash of the full generated name: one-shot Jobs fit the 63-byte
Job-label limit, CronJobs reserve Kubernetes' suffix within their 52-byte limit,
and ConfigMaps fit the 253-byte resource-name limit. Labels and their lookup
selectors use the same bounded value; owner references and report attribution
retain the full Eval name and UID.

Runner Pods require a non-root user, RuntimeDefault seccomp, no privilege
escalation and dropped capabilities for namespaces enforcing the restricted
Pod Security Standard. The shipped image declares UID 1000; custom images
retain their declared numeric non-root user rather than being forced to that
UID. Non-egress runs do not mount a Kubernetes service-account token.

For `EgressConnect` cases, the controller verifies the sandbox's namespace
claim and the Deployment/ReplicaSet/Pod ownership chain, then binds the run
to exactly one ready router Pod UID. The runner uses Kubernetes-authenticated
port-forwarding to that Pod's **loopback-only port 8444**. It sends CONNECT,
reads the proxy response, and closes the connection without tunneled payload.
The proxy is not exposed through a Service or rebound to a public address.

An Eval-owned service account receives only `get` on the named Pod and
`get`/`create` on its `pods/portforward` subresource. The maintained
`kars-eval-proxy-port` admission policy additionally restricts these accounts
to port 8444; RBAC alone cannot restrict port numbers. The controller refuses
to grant access unless the exact policy and enforcing binding are installed
and the policy has been type-checked. No exec, Secret access, router admin
token or cluster-wide runner role is granted, including for custom images.
Grants are owned by the target Pod, replaced when its incarnation changes,
and revoked during Eval finalization. The service account is collected with
the Eval, as are its UID-owned ConfigMaps, Jobs and CronJobs. Finalizer removal
is fenced by Eval UID and resource version and preserves other finalizers;
similarly named or labeled foreign resources are not cleanup targets.
Both scheduled and one-shot templates retain the same UID binding across
run-request acknowledgement.

The runner checks that UID before and after opening each port-forward, before
sending any probe; each request has a bounded timeout and cancels its forwarding
task on completion or error. Missing permissions, an unavailable proxy or a
replaced Pod are evaluation errors, not policy denials or passing cases.
Standalone runner invocations can still explicitly select a reachable proxy
with `--forward-proxy`; installed Jobs select `--forward-proxy-pod` together
with `--forward-proxy-pod-uid` instead.

For inconclusive Pod-proxy API failures, inspect the runner Job logs. Warnings
record `stage` (`target_get_before_upgrade`, `portforward_upgrade`, or
`target_get_after_upgrade`), `kube_error_category`, a static `upgrade_failure`
variant, and `http_status` when available. Diagnostics never render upstream
error text, response bodies, headers, tokens or credential-bearing URLs. Verdicts
are unchanged: a Kubernetes GET authentication denial remains `Authentication`,
while a rejected WebSocket upgrade remains `Transport`, even for HTTP 401/403.
An upgrade status is diagnostic evidence, not an egress-policy decision.

Use `governed-egress-tunnels` for targets with task-bound governed inference,
including unbounded, non-expiring allowances: their proxy refuses opaque tunnels
before evaluating destination allowlists. The controller enables this fence with
`KARS_INFERENCE_BUDGET_REQUIRED=true` and its task binding. Calendar-only accounting
(`KARS_CALENDAR_BUDGET_BINDING` without that task binding) does **not** enable the
fence; those targets still evaluate destination policies. The presence of an
accounting endpoint or an unbounded allowance alone is not this corpus's prerequisite. Select it in Operator Console → Safety evals
→ New eval; it does not require a runner override or a weaker network policy. This corpus
requires the actual governed-tunnel refusal for all six destinations; generic
403 responses, allowlist denials and transport errors do not satisfy it. The
report does not attribute this boundary refusal to an EgressAllowlist.

The separate `egress-known-bad` baseline remains available for installations
that permit the opaque proxy and enforce destination allowlists, not learn
mode. Learn mode permits discovery of other hosts. DNS failures and upstream
timeouts remain inconclusive. These corpora are not interchangeable evidence:
the evaluator never changes the target policy or silently switches expectations
to make a baseline pass. Enter the appropriate corpus name in Safety evals.

For inference cases, an upstream HTTP 400 is a policy denial only when the router
explicitly supplies `x-kars-decision: Blocked` (or `blocked`),
`x-kars-decision-by: InferencePolicy`, and a nonempty reason. Generic HTTP 400s,
authentication failures, and upstream or transport errors remain inconclusive.
This exception does not apply to memory requests. A successful model response
containing a textual refusal is still an `Allowed` transport decision, not a
router-enforced block. A jailbreak baseline mismatch must remain visible rather
than being converted to a pass based on generated text.

The Console's Safety evals page refreshes automatically while visible. An open
detailed report follows the latest completed run, and downloads exclude an older
cached report when the run changes. Egress cases show their CONNECT destination;
inconclusive cases retain the runner's error category (for example `Upstream` or
`Timeout`), and failed cases retain the declared mismatch reason. These explain
the recorded result without turning a transport error into a policy denial.

---

## Builtin corpora

Six corpora ship compiled into the controller binary and are
referenced by name via `spec.corpus.builtin`:

| Name | What it tests |
|---|---|
| `jailbreak-baseline` | Classic LLM jailbreak prompts (DAN, role-reversal, system-prompt extraction). Default if `spec.corpus` is omitted. |
| `prompt-injection-2026q1` | Indirect prompt injection via tool outputs, retrieved documents, and crafted user input. |
| `banned-tools` | Asserts the sandbox refuses calls to denylisted MCP tools (filesystem write outside `/sandbox/workspace`, raw shell, etc.). |
| `egress-known-bad` | Asserts the inference router blocks egress to known-bad hosts even when the agent is convinced to make the call. |
| `governed-egress-tunnels` | Asserts governed-inference opaque-tunnel refusal for the six egress destinations, including with unbounded allowances; does not claim destination-allowlist evaluation. |
| `memory-isolation` | Sends governed REST read probes against the bound store with explicit foreign `scope` arguments; requires an attributed scope-policy denial, not a wrong-store or generic HTTP access denial. |

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

The controller claims each explicit request before creating its Job. Its token
and requested Job name are stamped on the Pod template, and scheduled Jobs
inherit that acknowledged request context from their CronJob template. The
consumer checks those producer stamps together with native UID ownership and
the actual spec; names and labels alone are not attribution. A missing or
still-running explicitly requested Job keeps the eval Pending, rather than
falling back to an older success (including an older scheduled success).
When the requested Job is absent, a protected terminal receipt must prove
fulfillment of that exact Eval UID, request token and requested Job. The
controller does not guess whether a missing Job completed or merely disappeared.
That fulfillment can survive a corpus, runner, schedule or target intent change:
it releases the outstanding-request gate, but never makes the historical report
current. Only a newly attributed run of the current producer can restore Ready.
If the historical source Job still exists, its UID, generation, terminal time,
producer/request stamps and ownership are checked before observation and
again before publishing the new report. The protected receipt authenticates
the historical owner chain even if that CronJob was subsequently collected or
recreated; fresh scheduled reports must match the live CronJob UID.
Later scheduled receipts preserve the
verified request context, allowing repeated intent changes and Job/Pod GC
(including a historical Job already terminating)
without another manual trigger. An unauthenticated receipt or receipt from
another request cannot release the gate.

---

## Status — what to read

```
$ kubectl get karseval -A
NAMESPACE              NAME                SANDBOX     SCHEDULE      PHASE     LASTRUN   PASSED  FAILED  AGE
kars-my-agent     nightly-regression  my-agent    0 3 * * *     Ready     12h       42      0       3d
```

The printer columns (`Sandbox`, `Schedule`, `Phase`, `LastRun`,
`Passed`, `Failed`, `Age`) come straight off `status` and are the
fastest way to see "is my eval doing its job?".

### `status.phase`

Stamped only after checking current intent, terminal workload identity and persisted evidence:

| Phase | Meaning |
|---|---|
| `Pending` | CR has been admitted; no run has completed yet. Either the first run is in flight or `run-now` hasn't been set and there's no schedule. |
| `Ready` | A current, attributed v2 run evaluated the complete nonempty corpus, every case passed, and its bounded report was persisted. |
| `Degraded` | A current policy failure, inconclusive/error result, unavailable evidence, or legacy runner requiring upgrade. Only confirmed current policy failures can set sandbox `ConformanceDrift`; that write is UID/resourceVersion-fenced. |

### `status.conditions`

Three standard plus one KarsEval-specific:

| Type | When it goes `True` |
|---|---|
| `Ready` | Same trigger as phase=Ready. |
| `Progressing` | A run is in flight (Job exists and hasn't completed). |
| `Degraded` | Same trigger as phase=Degraded. |
| `ConformanceDrift` | Current attributed v2 evidence contains real policy failures. Transport failures and unverifiable legacy reports are not policy drift. A mixed run can be inconclusive overall and still contain a real policy failure. |

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

`status.reportConfigMapRef` points to the exclusively Eval-owned
`karseval-<name>-report` ConfigMap. `report.json` keeps bounded per-case
IDs, expected/actual symbolic decisions, verdict/error categories and durations;
the existing `pass`/`errored` case projections remain readable by Bridge.
`evidence.json` binds the Eval generation/UID, current intent, Job/Pod identities
and evidence digest. Combined retention is capped at 256 KiB and 512 cases.
The protected status also records `reportConfigMapUid` and
`reportEvidenceDigest`; cache-only replay after Job GC must match that prior
controller receipt as well as current intent. An unsigned ConfigMap alone
cannot create fresh Ready.
The receipt also binds the producer's request token and requested Job name.
It survives Pod/Job TTL cleanup without relabeling an old run as a new request.
If the same terminal Job remains but its Pods have been collected, its matching
protected receipt is reused unchanged; a replacement Job UID is not accepted.
A protected pre-stamp receipt remains usable after GC only when its existing
token, intent and source Job identify the same explicit request; a receipt
from a different Job cannot be retroactively bound to that request.
Raw prompts, response bodies, headers and free-form error details are not retained.
The latest report is written before history/readiness advances; a lost write
acknowledgement is retried idempotently. Foreign or malformed ConfigMaps are
preserved and surfaced as evidence errors, never adopted with force-SSA.

The source runner log remains accessible using the spawning Job name:

```bash
kubectl logs -n kars-system job/$(kubectl get karseval -n kars-system \
  nightly-regression -o jsonpath='{.status.lastResult.jobName}')
```

Successful inference responses also emit a case-bound `Inference safety observation`
line in the runner log. It contains only HTTP status and fixed-field counts: prompt
results, results with well-formed Prompt Shields annotations, detected and filtered
attack annotations, response choices, content-filtered choices, and structured
refusal choices. Missing annotations are distinct from valid negative detections.
No provider strings, prompts, model output, credentials, or response headers are
copied into these diagnostics, and their size does not grow with response content.

These observations do not change the HTTP-based verdict or the report format.
A provider response with a structured refusal still counts as HTTP `Allowed` when
its HTTP decision is allowed; absence of a structured refusal does not prove harmful
output. Keep a failed baseline when investigating detection drift; diagnostic
counts are not a substitute for policy enforcement or output-safety assessment.

### Corpus digest drift

`status.corpusDigest` is the SHA-256 of the resolved corpus bytes
**as the controller saw them**. If a builtin corpus is updated by a
controller upgrade, or a signed bundle in the registry rotates to a
new digest, this field will change on the next reconcile. Combined
with `lastResult.corpusDigest` this lets operators answer
"did the corpus drift, or did the sandbox drift?" without leaving
`kubectl`.

The corpus is materialised into an ownership-checked ConfigMap and mounted into
the runner pod. The runner reports its hash of the bytes it actually read.
The consumer rejects a v2 report whose digest, corpus name, router, case
inventory, timestamps or native workload identity disagrees with the current
intent. It never replaces the reported digest with a newly resolved digest.
Kubernetes `metav1.Time` exposes container completion at whole-second
precision. Fractional report completion within that same second is accepted;
the start must still be at or after Job creation, and completion in the next
second is rejected. Report interval, exit-code and native identity checks
remain mandatory.

### Report negotiation and consumer-first rollout

The actual Job and CronJob builder sets `KARS_EVAL_REPORT_FORMAT=v2`. There is
no new mandatory CLI option, image pin, or UID override. Custom runner images
with an ordinary numeric non-root USER remain supported.

| Controller / runner | Result |
|---|---|
| Older controller / new runner | No format environment: conclusive healthy or policy-failing runs retain v1 output. Inconclusive or empty runs exit 2 without fabricating a v1 report. |
| New controller / new runner | The environment selects strict v2, including `Errored` with no actual decision and a bounded category. |
| New controller / older or custom v1 runner | The image can still run. Valid v1 results/history stay readable, but Ready remains false with `RunnerUpgradeRequired`, not fabricated policy drift. Upgrade the runner for qualification. |

Deploy the strict consumer before using negotiated v2 runners. An older
consumer's loose version handling is not a safety boundary. Existing unbound
pre-upgrade Jobs are not silently adopted as current evidence; existing history
and exclusively owned legacy reports remain readable.

Exit codes are 0 only for a nonempty all-pass run, 1 for conclusive policy
failure, and 2 whenever any case is inconclusive (including mixed runs), the
selection is empty, or execution/reporting fails. HTTP authentication failures,
5xx, malformed or incomplete responses, and failed CONNECT/burst requests are
inconclusive. MCP JSON-RPC errors and `isError` text are not guessed into policy
decisions. An MCP HTTP 403 without both `X-Kars-Decision: Blocked` and a valid
`X-Kars-Decision-By` is an inconclusive access failure, not a tool or memory
policy verdict. MCP policy kinds are never inferred from the requested scenario,
including burst observations. `reasonContains` is evaluated internally without
persisting response text.

MCP probes negotiate `Accept: application/json, text/event-stream`. Memory
probes send `scope` as a tool argument, separately from search text. Scope
partitions within a bound store are supported by both runtimes; a foreign-looking
partition name alone does not prove access to another sandbox's store. The
memory corpus must not be treated as proof of store isolation without the
corresponding binding/policy evidence. The router's local socket restriction
still applies: a separate runner Pod calling its Service cannot exercise local
MCP, and that refusal must not count as safety acceptance. Do not grant the
egress-only evaluator forwarding access to the privileged router port to bypass
this boundary.

Only native Job `Complete=True` or `Failed=True` conditions are terminal;
failed-attempt counters alone are not. Eval/Job/Pod UID, generation, owner and
spec continuity are rechecked around log reads and persistence. Changing intent
or requesting a new run cannot promote stale history to a fresh pass.
Sandbox drift publication preserves every unrelated condition, upserts only
`Degraded`, records the observed sandbox generation and preserves transition
time while its status stays True. Its read/modify/write remains fenced by
the target UID and resourceVersion, so a conflict cannot overwrite a
concurrent sandbox update.
These rules do not claim runner-image execution or native lifecycle
qualification from source-only tests.

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
