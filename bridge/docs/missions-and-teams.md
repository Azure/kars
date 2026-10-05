<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Missions & teams

Bridge runs work as either a one-shot **mission** or a standing **team**, both on
the same governed kars substrate.

## Missions (one-shot)

A mission is a `KarsTask` with a trust envelope (tier, budget, tools, egress,
delegation depth). The composer turns a plain-language objective into an editable
launch package. For supported planless execution, the controller materializes a
sandbox and the run delivers the objective into the agent's loop over the AGT
mesh. A matching terminal result, producer and actual artifact bytes establish
delivery—not a launch acknowledgement. Task-level signed receipts do not by
themselves establish revision-bound delivery. A mission can delegate within its
envelope—see [Observability → agent graph](observability.md).

Reviewed typed execution plans are preserved but currently cannot be activated:
`TypedPlanExecutionUnavailable` prevents silently substituting objective-only
execution for reviewed roles and phases. See the
[current execution boundary](team-workflows.md#current-execution-boundary).

## Teams (standing)

A `KarsTeam` is a standing org with a charter and an optional cadence. For
supported active Teams, a cadence tick or admitted **Run now** request creates a
governed task-force Task. A manual request is durably reserved before activation
and acknowledged without replaying a completed request. Neither admission nor
an idle roster proves delivery. New typed-plan Teams must remain paused until
plan-aware execution is implemented.

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

### Task backlog

Beyond the standing charter, a Team can hold a **task backlog** of discrete
milestones. Automatic claiming, dependency advancement and review continuation
are not yet connected to the durable manual-run path. A queued engineering item
therefore does not prove an agent has started, delivered or advanced that item.

## Deliverables

Both surfaces render deliverables via the shared, typed deliverable view (report /
recommendation / action / note, with an "in brief" summary). A **pull request** the
agent opened is a first-class delivery type, shown as an artifact chip (repo +
number + link) — see [Connections → GitHub](connections.md).

### Artifact viewing and downloads

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
