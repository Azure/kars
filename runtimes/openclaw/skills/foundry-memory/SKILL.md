---
name: foundry-memory
description: Governed persistent memory through the Kars router: configured Foundry or Bridge-owned private lexical memory. No runtime-local fallback.
---
<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Governed persistent memory

Use the `foundry_memory` tool (platform name `foundry.memory`) for persistent
preferences, decisions and context. The compatibility name does not mean a
Foundry project must be configured. In reviewed Teams/Missions, the phase must
have the native `memory` capability. No external MCP server or network capability
is needed just for memory.

The router selects a configured/discovered Foundry project; without one and
without an explicit binding, it uses Bridge-owned private built-in memory.
The selected inference model/provider does not select memory. Never switch to
local files after an authorization failure, outage or storage-capacity error.

## Operations

- Search: `{"operation":"search","text":"coffee preference","top_k":10}`.
- Static recall: `{"operation":"search","top_k":20}`.
- Update: `{"operation":"update","text":"The user prefers dark roast coffee."}`.

Omit `scope` or use null. The router derives the authorized private scope. Do not
invent a mission scope, use `$SANDBOX_NAME`, or construct a per-user partition.
An explicit scope must match the private scope or an explicitly granted Foundry
shared scope. Do not create arbitrary stores or issue raw memory REST requests.
Reviewed native memory permits search/update, not deletion.

Results identify `backend`. Foundry can return queued/in_progress updates; these
are not proof of persistence. `completed` means acknowledged storage, not human
approval of knowledge. Read back when persistence matters. Do not blindly replay
an update after timeout or a denied status poll: its outcome may be unknown.

Built-in memory returns `retrieval: "lexical"`: it matches words, not semantic
embeddings. Submit concise explicit facts and query matching words. It stores
exact submitted text, deduplicates identical updates, and does not automatically
extract facts. Limits are 512 records, 16 KiB per text, 900 KiB serialized per
private store; reaching capacity preserves existing records rather than evicting.
It invokes no model and does not fabricate inference usage.

## Lifetime and privacy

Storage is outside the runtime pod. Built-in reviewed Team memory belongs to the
exact Team UID and role; synthesis is separate. Standalone Mission memory belongs
to the root Task UID and role. It survives pod replacement and phases/retries;
Team roles retain it across runs. Deleting the owner deletes its private memory.
Other roles cannot read it unless facts are deliberately handed off through the
approved workflow. Existing Foundry bindings/scopes retain their own semantics.
Private memories are not automatically shared or promoted into Team commons,
approved knowledge or deliverables.

Use document search for source material, normal handoff files for outputs and
web tools for fresh external information. Do not mistake lexical recall for
semantic retrieval or private memory for reviewed evidence.
