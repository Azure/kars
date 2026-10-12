<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Missions & teams

Bridge runs work as either a one-shot **mission** or a standing **team**, both on
the same governed kars substrate.

Home suggests a lifecycle from the request's wording; this is not a model
confidence score. Explicit one-off work stays a Mission, including research
that needs multiple agents, tools or roles. A mention of a team or a negated
recurring team is not, by itself, a request for standing work. Mission/Team
selection remains editable before composition and is labelled as your selection
when overridden. While navigation is pending, intake controls are disabled.
An explicit request to maintain or create a standing team takes precedence over
the report, research and review verbs describing its recurring deliverables.
This applies to noncoding charters as well as repository work.

Loop suggestions are constrained to the chosen lifecycle: Standing Watch belongs
to Teams, while Explore-Branch belongs to Missions. An incompatible model choice
is reported and replaced by a labelled keyword suggestion, not presented as a
valid model-authored proposal. The designer also explains unsupported saved
proposals and requires a compatible selection before applying them.
Reviewed execution rejection identifies the exact role, phase, capabilities and
unsupported tool names rather than listing every possible restriction. Tool
argument values are not included in that diagnostic. Rejection never removes a
requested capability or increases a limit to make a proposal launchable.
Team composition reserves the principal for final synthesis rather than creating
a duplicate member that disappears from the editable roster. Requested filenames
belong in the typed deliverable contract, not only in milestone prose. Model-route
fallback selection preserves explicit member harness choices.

## Missions (one-shot)

A mission is a `KarsTask` with a trust envelope (tier, budget, tools, egress,
delegation depth). The composer turns a plain-language objective into an editable
launch package. For supported planless execution, the controller materializes a
sandbox and the run delivers the objective into the agent's loop over the AGT
mesh. A matching terminal result, producer and actual artifact bytes establish
delivery—not a launch acknowledgement. Task-level signed receipts do not by
themselves establish revision-bound delivery. A mission can delegate within its
envelope—see [Observability → agent graph](observability.md).

Supported reviewed filesystem plans execute their reserved roles and phases,
then synthesize from their immutable named handoffs. The composer's
`execution-plan/v1` marker is eligible only with a pinned typed plan; the
dispatcher still reconstructs and verifies the complete reviewed authority,
installed contract, current revision and input custody before execution.
Unsupported plans cannot fall back to objective-only execution. See the
[current execution boundary](team-workflows.md#current-execution-boundary).

Both runtime adapters request another authorized tool operation while a reviewed
phase's successful-call minimum is unmet and attempts remain. Failed operations
never count as successes. The model sees the remaining allowance and must perform
useful work or verify its written findings; inline facts do not imply input files.
The existing attempt, inference and funding ceilings and final evidence checks
remain authoritative; this does not replay a failed assignment.

Hermes replies use the Python mesh client's JSON-string envelope. The dispatcher
decodes one bounded envelope only for a Hermes-bound candidate, then applies the
same encrypted-peer, mission, phase, boot and assignment checks as object replies.
Malformed or mismatched readiness never creates an execution claim.

Hermes mission subprocesses retain the sealed image's no-install posture in
their clean environment and configuration. Native imports cannot bootstrap
optional provider packages, Node.js or Tirith at runtime; required dependencies
must already be present in the immutable image.

Concurrent Team updates retry optimistic Kubernetes conflicts without replacing
the current progress with a terminal `Degraded` status. Other reconciliation
failures still surface as errors.

The Mission Activity graph and drill-down include revision-bound reviewed
phases using the same evidence projection as Team runs. A phase's files and
successful tool results are not proof that root synthesis has completed.
The deployment timeline recognizes bound phase router activity before principal
dispatch, without claiming that the principal sandbox is ready or has delivered.

Retained phases with verified idle execution and no sandbox are reconciled every
minute rather than at active-run speed. Active funded work and pending admission
or cleanup retain their ten-second reconciliation interval; launch and inference
requests still verify current authority. Unchanged Task status is not rewritten.
Each controller admits at most four simultaneous Task reconciliations, so a
restart or synchronized idle check queues retained lineages rather than starting
all of their authority reads at once.

Mission and Team composers leave model fallbacks empty unless requested. Proposed
fallbacks retain their order and must independently satisfy the complete package's
catalogue, resource, qualification and budget requirements. Available catalogue
models are not automatically selected as fallbacks.

Budget preflight rejects a parent or reviewed role cap that cannot reserve even
one request under the operator's complete-input bound. This is not an estimate
of prompt size: generated output and prior usage need additional headroom.
Team activation, Resume and Run now check the same contracts, including explicit
roster model overrides. A paused draft can still be corrected and Pause remains
available. Changing a role allocation never increases or resets the Team's
immutable lifetime account.

## Teams (standing)

A `KarsTeam` is a standing org with a charter and an optional cadence. For
supported active Teams, a cadence tick or admitted **Run now** request creates a
governed task-force Task. A manual request is durably reserved before activation
and acknowledged without replaying a completed request. Neither admission nor
an idle roster proves delivery. Manual and cadence runs use the same bounded
name prefix, leaving room for the run nonce and the sandbox namespace's `kars-`
prefix within Kubernetes' 63-character limit. Reviewed runs seal each named roster role's
blueprint and attenuated envelope with the run request. Role phases inherit that
runtime, model, policy and budget rather than silently using the principal's
runtime. Editing the Team affects subsequent requests, not an already reserved
phase. Older reviewed Team runs without a sealed roster cannot be resumed as new
work; request a fresh run while retaining their history.
The dispatcher and Bridge phase projection verify the same persisted roster
and retained Team-memory bindings against the root run's sealed annotations.
Later runs with retained documents keep their live phase projection; a changed
or missing memory binding fails closed instead of mixing revisions.
Acknowledgment retries are limited to three complete read-only authority
traversals when a Kubernetes snapshot changes mid-read. Every retry reloads and
revalidates the Task and sealed inputs; it never resends an assignment. Revoked
authority and persistent snapshot changes stop the attempt, with the failed
invariant retained in its error record.
Role overrides do not bypass reserved child identity, specification,
revision or immutable-input checks; a tighter roster allocation stays tighter
than the plan's role budget.

Pausing stops current runs. When a stopped run has no completed output, its exact
backlog claim returns to pending with a retained cancellation record. An explicit
Halt preserves its supplied stop time and reason; a plain Pause records when the
controller observed cancellation, not a fabricated runtime completion time.
Resuming and requesting another run uses a fresh run identity and keeps the same
Team lifetime budget and prior attempt history. Completed output still follows
the normal review and harvest path.

### Team profiles

Console **Agent capabilities** saves reusable `KarsProfile` templates. A successful
save is not controller validation or execution: inspect the reported profile
status before use. Missing status is awaiting validation, not Ready. The editor
checks basic envelope bounds and preserves raw JSON and role instructions on
failed saves.

Team setup copies a same-workspace Ready profile into an editable draft; it is
not a live binding to subsequent profile edits or an AI-composed proposal.
Representable tier, authority ceiling and delegation depth are retained explicitly.
The server still checks the execution plan and delegation requirements before
launch; a narrow profile is not silently increased to make execution succeed.
Missing/unreadable/degraded profiles and unavailable skills stop setup instead
of falling back to a different template or discarding skills.

**Current limitation:** the Bridge team-create API cannot preserve profile
envelope-only `toolPolicyRef`, `budget`, `egressAllowlistRef` or unknown envelope
settings. Team setup refuses those profiles rather than dropping their
constraints. This is an open integration gate, not evidence that all profile
features are operational. Team-level bounding tool policy and an explicitly
reviewed lifetime token budget remain separate supported inputs.

### Operating mode — always-on vs on/off

Standing intent and runtime retention are separate. The configured lifecycle
policy controls whether a principal remains ready, stays warm for an idle window,
or is torn down between assignments. Pausing explicitly suspends retained
runtimes. See [runtime lifecycle modes](team-workflows.md#runtime-lifecycle-modes).

An operating-mode badge describes runtime state, not whether the reviewed plan
was executed or a useful deliverable exists. Verify the exact run and its
producer/result evidence before treating work as complete.

### Memory across runs

The team's **knowledge commons** (`kars-commons-<team>`) carries bounded prior
results across runs independently of sandbox lifetime. Harvesting requires the
matching Task UID and completed revision, and the next run can include recent
entries as context. A harvested result or memory count does not establish human
approval or useful content; those require inspection of the underlying evidence.

Commons and archive reads use the Team's actual namespace and configured store,
verify exclusive ownership by its current UID, and check every indexed content
digest and byte count. Unavailable or malformed stores are errors, not empty
knowledge. The API's `digest` and `size_bytes` describe the returned, readable
content; `stored_digest` and `stored_size_bytes` separately identify the original
stored bytes when a legacy agent envelope is unwrapped. A run archive resolves
the newest retained entry for that run. Both content addresses preserve the
existing `sha256:` plus first 16 hash bytes format; they are not full-length
SHA-256 digests.

Reviewed Team runs also retain complete named documents and final peer handoffs
in the same Team-owned store, with full SHA-256 hashes and original Task UID,
run nonce, assignment, pod and agent provenance. The controller verifies the
sealed run, durable terminal journal and immutable canonical projections before
copying bytes. Adding an archive to an older summary does not rewrite its
audited text, digest or timestamp. The archive survives disposable Task removal;
historical reads cannot authorize execution.

New manual and cadence admissions seal complete recent archive bundles as
read-only mission input artifacts for both OpenClaw and Hermes. At most eight
memory files and 32 KiB are admitted, reserving room within the existing
16-file/64-KiB protocol limit for new peer handoffs. Selection considers the five
newest entries, newest first, without splitting a run's bundle. If the newest
available complete bundle cannot fit, admission fails rather than recalling
truncated documents. Legacy summaries remain bounded reference context, not
evidence that full documents were retained. Old summary/archive pairs are
pruned together within the 64-entry, 900-KiB store budget. Memory and archived
run pages expose verified full-file downloads separately from summary digests.

### Task backlog

Beyond the standing charter, a Team can hold a **task backlog** of discrete
milestones. Admission binds an eligible item to an exact Task UID and run nonce;
dependencies and requested human review gate advancement. A queue write or run
request alone does not prove an agent has started or delivered that item. See
[work ownership and review](team-workflows.md#work-ownership-and-review).

Console **Approvals** includes fleet-wide authority requests and checkpoints,
separately from temporary egress grants. Team milestone denial requires written
feedback. Clarification questions remain in the owning user's Workspace inbox.
If either approval queue cannot be read, decisions are unavailable; a read failure
must not appear as an empty queue.

## Deliverables

Both surfaces render deliverables via the shared, typed deliverable view (report /
recommendation / action / note, with an "in brief" summary). A **pull request** the
agent opened is a first-class delivery type, shown as an artifact chip (repo +
number + link) — see [Connections → GitHub](connections.md).

### Artifact viewing and downloads

The global Deliverables index joins modern output and artifact records to the
exact live Task namespace, UID, owner and run nonce. It does not require legacy
ownership fields or file manifests on modern records. Current committed revisions
link to the Mission's download/review surface; historical revisions remain distinct
and cannot inherit the current approval. Internal child Tasks stay in their
parent's drill-down and All records, not the standalone Mission list. Read failures
are errors, not evidence of an empty index.
Historical revisions are visibly labeled. Revealing internal evidence does not
change the card's customer-file type; file counts reflect the selected view.

OpenClaw's encrypted mission path exports explicitly named UTF-8 files through
`file_write.artifact_name`; null or omitted names remain private scratch. A parsed
`file_write` call with a non-null artifact name requests an export. Rejection or
failure remains unresolved while normal model/tool retries continue within the
existing round and phase limits. Only a later successful committed export with
the same valid artifact name clears that failure: an older commit, a different
filename, scratch writes, or inline content cannot satisfy it. Invalid non-null
names remain unsatisfied. A final response with any unresolved export fails with
its existing usage/phase evidence, without delivering partial or stale attachments.
Successful writes still commit their exact text; private scratch and genuinely
inline-only delivery remain supported without inferred export requirements. Both exported
text and inline deliverables preserve Unicode characters across HTTP response
chunk boundaries. An attachment claim in the final response is not proof of a
retained file—review the actual artifact list and downloaded bytes.

A stopped Mission keeps its current deliverable reviewable and approvable.
Requesting changes records a distinct revision without launching stopped compute;
launch that revision explicitly from **Overview** after reviewing its gates.
Relaunching a runtime does not itself establish that a new revision is executing.
Current terminal evidence takes precedence over stale live assignment state.

Mission feedback carries the original objective, complete previous response and
the current revision's text files, not their UI previews. Modern counted records
and legacy file manifests are supported; missing counted files fail closed. Router
evidence and the duplicate `response.md` export are excluded from the file context.
The Task UID, completed nonce and resource-version check bind the revision request;
missing, stale, non-text or size-mismatched files prevent it rather than silently
discarding context. The complete JSON-encoded revision objective must fit 64KiB,
leaving room for reviewed handoffs and the existing mission wire envelope. An
oversized revision is rejected before changing the Task or review journal.
Reviewed phases retain the Task schema's 4,096-character objective limit. Larger
phase instructions use the existing run-nonce/digest-bound objective annotations,
with a bounded role/phase description in the Task spec. Both controller admission
and dispatcher authorization re-derive the complete objective and check its
annotations against the reserved plan; neither truncates context nor weakens the
schema.

Mission and team-run artifact links share
`GET /api/namespaces/{ns}/tasks/{name}/artifact/{file}?run_nonce={revision}`.
The current endpoint requires a live owned Task in `kars-system`, its exact UID
and matching current completed revision. Missing revisions return 400, stale or
incomplete revisions 409, and missing or inaccessible Tasks/artifacts 404. It
does not provide historical downloads after Task deletion. Retained evidence is
a separate capability, not authorization to serve a name-only match.
Artifact bytes are untrusted agent output: HTML, SVG, PDF, and unknown formats
are served as **attachments**, even when using **Open** rather than **Download**.
Passive text/structured-data formats and PNG/JPEG retain inline viewing.
Downloads preserve the original bytes; file names are sanitized for headers.

Every raw artifact response carries `X-Content-Type-Options: nosniff`, private
no-store caching, and a restrictive CSP with `sandbox` (no script or same-origin
permissions), blocked external resources, forms, base URLs, and framing. The
same-origin web API proxy preserves these headers and streams bytes unchanged.
This response boundary—not `noopener` or a link's `download` attribute—prevents
agent-produced executable content from inheriting the authenticated Bridge
origin. Downloaded files remain untrusted; inspect them before opening locally.

For the detailed relationship between engineering intake, backlog milestones,
runs, activity, role artifacts, principal deliverables, review gates,
checkpoints, and team memory, see
[Team workflows: from intent to reviewed outcome](team-workflows.md).
