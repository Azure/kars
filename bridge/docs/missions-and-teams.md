# Missions & teams

Bridge runs work as either a one-shot **mission** or a standing **team**, both on
the same governed kars substrate.

## Missions (one-shot)

A mission is a `KarsTask` with a trust envelope (tier, budget, tools, egress,
delegation depth). The composer turns a plain-language objective into an editable
launch package; on launch the controller materializes a sandbox, and the run
delivers the objective into the agent's loop over the AGT mesh. The deliverable +
a signed receipt come back. A mission can delegate to sub-agents (bounded by its
envelope) — see [Observability → agent graph](observability.md).

## Teams (standing)

A `KarsTeam` is a standing org with a charter and an optional cadence. Each
cadence tick (or "Run now") mints one governed task-force run; the run can spawn
sub-agents just like a mission's principal.

### Operating mode — always-on vs on/off

A standing team does **not** hold an always-on sandbox. It materializes a fresh,
governed sandbox per run and **tears it down on completion** to stay lean. The
team page surfaces this honestly with a live operating-mode badge:

| Mode | Meaning |
|------|---------|
| **Working now** | A run sandbox is live and executing the charter. |
| **Idle — spins up on demand** | No sandbox is running between runs; it will spin up on the next tick/task/Run now and **rebuild from memory**. |
| **Hibernating** | Paused — nothing runs until resumed. |

"Always-on in intent, on-demand in cost."

### Memory across runs

Because runs are ephemeral, continuity comes from the team's **knowledge commons**
(`kars-commons-<team>`). Each delivered run harvests its deliverable into the
commons (when it did real work), and the next run injects recent commons entries
as prior knowledge — so a team resumes from what it has learned rather than a
cold start. The operating-mode explainer shows the count of carried-forward
memories.

### Task backlog

Beyond the always-on charter, a team can hold a **task backlog** (discrete tasks,
one per run). Each run claims the oldest pending task, works it, and marks it done
on delivery.

## Deliverables

Both surfaces render deliverables via the shared, typed deliverable view (report /
recommendation / action / note, with an "in brief" summary). A **pull request** the
agent opened is a first-class delivery type, shown as an artifact chip (repo +
number + link) — see [Connections → GitHub](connections.md).

For the detailed relationship between engineering intake, backlog milestones,
runs, activity, role artifacts, principal deliverables, review gates,
checkpoints, and team memory, see
[Team workflows: from intent to reviewed outcome](team-workflows.md).
