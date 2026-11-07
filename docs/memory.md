<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Governed persistent memory

OpenClaw and Hermes use the same router-controlled memory operations in reviewed
Teams and Missions. The compatibility tool names remain `foundry_memory` and
`foundry.memory`; the name does **not** require a Foundry inference provider.
Grant the native `memory` capability, then search/update through that tool.
Memory alone does not require the `network` capability or an external MCP server.

## Backend selection

- A configured/discovered **Foundry project endpoint** selects Foundry Memory
  Store. This is independent of the inference provider: Azure OpenAI inference
  alone is not evidence that a Foundry project exists.
- Without that project and without an explicit binding, the router selects
  **Bridge-owned built-in memory**. No database or runtime-local memory plugin is
  needed. Bridge discovery reports `memory_backend`; tool results report
  `backend: "foundry"` or `backend: "builtin"`.
- An explicit `KarsMemory` remains a Foundry-only resource with exact private and
  shared grants. Missing, malformed or unavailable required bindings deny access;
  they never enable built-in memory. Composition does not auto-select a binding:
  use `memory: null` for the default or deliberately select a listed binding.
- Authorization errors, timeouts, exhausted storage and Foundry outages never
  change backends. Selection is pinned for the router process; a binding reload
  that changes it is denied. A deliberate configuration change requires a router
  rollout. Neither records nor scopes are automatically migrated or deleted.
- If discovery cannot read the controller configuration or resolve an indirect
  project setting, it reports an error, not an inferred absence of Foundry.

Omit `scope` (or use null) to use the router-authorized private scope. Never invent
scope names. Additional shared scopes require an explicit Foundry binding. Existing
Foundry bindings, conventional scopes and records retain their previous semantics;
use explicit bindings for continuity across independently named Foundry sandboxes.
When applying a binding with `kars memory apply`, `--no-delete-on-sandbox-delete`
sets `spec.deleteOnSandboxDelete: false` to request retention of its Foundry
contents. Omitting the flag preserves the cleanup default. This retention setting
does not grant another sandbox access or replace an explicit private/shared grant.

## Team member bindings

Selecting **Team runtime memory** binds the principal and bounds which memory
resource members may select. In each member's resource controls, explicitly select
that same memory binding to share it with the member. The selection survives roster
editing and is sealed into fresh reviewed runs for both OpenClaw and Hermes. A
member cannot select another resource outside the reviewed Team binding.

A runtime/model override does not implicitly inherit the Team's memory. With no
member binding, that member uses default backend selection and its own authorized
scope. Existing explicit CRD role blueprints remain complete overrides; no sharing
is added automatically. Remove member selections before removing or changing the
Team binding. Changes apply to fresh runs, not retained execution identities, and
never migrate existing per-run records into the newly selected scope.

## Foundry recall and original records

Foundry processes submitted text into extracted memories, such as `user_profile`
facts. A completed update acknowledges that processing; it does not promise to
store the original observation verbatim. Search can return those extracted facts
with their memory IDs while omitting original labels, timestamps or exact wording.
Check returned contents and IDs separately from update completion, and preserve
original evidence in retained artifacts when exact text matters. Same-run recall
alone does not prove later-run continuity or owner approval.

This differs from built-in memory, which retains submitted text and uses lexical
recall. Neither backend is the Team's shared knowledge commons, and Foundry's
extraction semantics are not a reason to silently switch backends.

Approved Team commons summaries retain their original bytes and provenance on
controller retries, including whitespace at a summary or metadata truncation
boundary. A retry does not rewrite the approved record; conflicting content or
provenance remains an error.

## Adding members to an existing Team

Open **Org & access → Edit org**, pause the Team from **Overview**, and add a
member or an archetype. Review its instructions, **Wait for** dependencies and
phase permissions, then **Save org**. Membership and the corresponding execution
plan are saved together. Existing role limits, phase requirements, final synthesis
and deliverables are preserved. Removing a member that still supplies another
member's inputs requires explicitly changing those dependencies first. Saving
does not launch work or modify an already admitted run.

New members start with permission to read shared Team knowledge and previous
work, and to write findings. Their initial tool-call headroom comes from the
existing Team plan and is shown for review. Approved commons knowledge is supplied
automatically with their first fresh assignment, subject to the existing retained
input selection and capacity checks. It is a provenance-checked snapshot, not a
live update to running agents or a copy of every historical artifact. Pending or
unapproved outcomes are not automatically promoted to shared knowledge. An
administrator policy can still deny reading; member permissions do not override it.

This shared commons access does **not** grant another member's private runtime
memory, another Team's knowledge, or a separately configured external memory
binding. Those resource bindings retain their explicit reviewed selection.

## Built-in persistence and privacy

The controller provisions a separate, bounded Kubernetes ConfigMap in the owning
workspace. It is **not** the Team commons/approved-knowledge archive. Its identity
includes the workspace UID, exact owner UID and private principal:

| Execution | Owner and private principal |
|---|---|
| Reviewed Team | Team UID + stable roster role name; synthesis is separate |
| Reviewed standalone Mission | Root Task UID + role name; synthesis is separate |
| Unreviewed Task | That Task UID, never implicitly promoted to Team ownership |
| Standalone Sandbox | Sandbox UID |

This storage survives router/runtime/pod replacement. Team role memory survives
successive runs of that Team. Mission memory survives its phases and retries while
the root Task exists. Deleting the owning Team, Task or Sandbox garbage-collects
its private records; recreating an identically named owner does not adopt them.
Role memory is not automatically readable by other roles or synthesis. Approved
handoffs and prior knowledge continue through their existing separate review path.

The router alone receives a short-lived, Secret-bound Kubernetes token granting
`get`/`update` on exactly one ConfigMap. Agents do not receive that token, a mounted
memory file or an ambient memory ServiceAccount. Owner references, pinned map UID,
resource version, identity and record digests are checked on reads and writes.
Concurrent writers use optimistic retries; failed writes do not evict records.
Before issuing or renewing this token, the controller revalidates the owned control
credential against both the SRE privacy qualification and the private-consumption
qualification. These have independent epochs; neither substitutes for the other.
Retired credentials, stale stamps and changed Sandbox or namespace ownership deny
reuse.
If a reviewed Task changes during memory provisioning, the controller discards
that stale snapshot and retries reconciliation before mounting memory or applying
the pod. Task replacement, deletion and failed authority checks still deny access;
a concurrent resource-version change alone is not a memory degradation.
When a valid control credential rotates, the controller revokes the old bound-token
anchor using its UID and resource version, then retries before issuing replacement
memory credentials or applying the pod. Successful revocation is a provisioning
transition, not a terminal reviewed-phase failure. Failed revocation and ownership
or authorization errors still block execution; retained private records are unchanged.
Existing governance and native capability/reservation checks still apply. Privileged
workspace/cluster administrators remain within the Kubernetes trust boundary.

Built-in recall is lexical word overlap, **not** semantic embedding or automatic
fact extraction. Empty searches list newest records; ties favor newest insertion.
Updates retain the submitted text; identical text is idempotent. Capacity is 512
records, 16 KiB per text and 900 KiB serialized per private store. Capacity errors
preserve prior records, with no silent eviction or fallback. Native reviewed tools
permit search/update, not deletion. A `completed` result confirms the acknowledged
write, not knowledge approval. Read back whenever persistence matters; a transport
failure can leave a write outcome unknown and must not be reported as success.

Built-in operations invoke no model and invent no inference-token charge. Tool
telemetry remains active. Foundry operations retain their existing actual usage
accounting and asynchronous update-status checks. Because Foundry can invoke models
internally without a provider-enforced bound, governed Foundry memory requires no
effective token or monetary ceiling on the live account or any task ancestor. A
normally reviewed uncapped Team or Mission needs no separate allowance override.
Cold-start built-in accounts accept explicitly unbounded funding only through a
matching validated grant; an empty account cannot authorize memory or inference.
Finite production ceilings, funding expiry and revoked authority remain enforced.
The broker reports this permission separately from calendar allowance overrides;
an older broker that omits it denies governed Foundry memory. Conventional private
partitions are revalidated and audited distinctly from explicit binding digests.

## Installation and qualification

Install the updated core Helm chart before enabling unbound built-in memory. It
creates `kars-memory-system` and the `kars-memory-accounts` admission policy and
binding. No Pods may run there, and only the core controller can mint its tokens.
The controller verifies the exact policy, observed generation and type-checking
before issuing credentials. Install the matching controller and router, then both
runtime packages for the updated descriptions/skill. Missing protection produces
`BuiltinMemoryNotReady`, never an insecure fallback. No Foundry data migration is
performed. Kubernetes persistence remains subject to the cluster's backup policy.

Local regressions and source validation are not deployed customer acceptance.
Qualify both runtimes through compose, review, execution and read-back, including a
fresh Team run, Mission phase continuity, cross-role denial, pod replacement and
configured-Foundry failure without backend switching.
