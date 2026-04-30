# Phase 2 — S15.g.2 Skills Move

**Date:** 2026-04-29
**Branch:** `phase2-skills-move-g2`
**Slice:** S15.g.2

## Scope

Mechanical relocation: `cli/skills/` → `runtimes/openclaw/skills/`.

## Rationale

SKILL.md is an OpenClaw-specific contract (the OpenClaw agent loads SKILL.md
files from `/opt/azureclaw-plugin/skills/` at runtime). When future runtime
adapters ship (`runtimes/openai-agents/`, `runtimes/maf/`), they will use
their own skill formats, not OpenClaw's. So the OpenClaw skill set belongs
alongside the OpenClaw runtime adapter package, not under the operator CLI.

This is a follow-up to S15.g.1 which moved the OpenClaw plugin source into
`runtimes/openclaw/src/`.

## Changes

- `git mv cli/skills runtimes/openclaw/skills` — 10 skill directories moved
  with all subfiles preserved.
- `sandbox-images/openclaw/Dockerfile` line 47: `COPY cli/skills/ …` →
  `COPY runtimes/openclaw/skills/ …`. Sandbox image runtime path
  (`/opt/azureclaw-plugin/skills/`) is unchanged.
- `CONTRIBUTING.md` layout table updated.

## Security considerations

None — pure file relocation. No source code, no permissions, no execution
paths changed. The sandbox image still copies the same files into the same
runtime path with the same ownership/mode bits.

## Verification

- `cli` build + tests: green (354 pass).
- `runtimes/openclaw` build + tests: green (100 pass).
- `grep -rn "cli/skills"` in source tree: no remaining matches outside
  CHANGELOG / audit-doc history.


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
