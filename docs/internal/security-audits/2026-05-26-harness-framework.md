# Security Audit — e2e-harness framework + local-k8s 9/9

**Scope**: PR #339 — `feat/e2e-harness-framework` (successor to the
auto-closed #337). Refactors the exec-brief harness into a pluggable
scenario+platform framework, adds the `file_read` tool, stabilises
sub-agents to 9/9 on local-k8s, and weaves observability + security
cross-links into the walkthrough docs.

Capability-introducing files touched (same set as #331; this PR makes
**no new capability changes** on top of what #331 already covered):

- `runtimes/openclaw/src/core/agt-task-loop.ts` — adds the `file_read`
  tool registration to the sub-agent task loop. The handler delegates
  to the existing OpenClaw file-IO surface, which is already
  router-gated and seccomp-bounded.
- `runtimes/openclaw/src/core/agt-task-tools.ts` — minor robustness
  improvements to peer-roster prepend (already audited under #331).

This document references **`2026-05-26-exec-brief-harness.md`** (PR
#331, squashed into main) for the substantive audit of the harness
runtime and prompt copy.

## 1. What changed

### 1a. Pluggable harness (`tools/e2e-harness/`)

`tools/e2e-harness/` (new dir, not in capability paths): scenario
files for exec-brief; platform plugins for `aks`, `local-k8s`, and a
`docker` stub. The harness applies `KarsSandbox` CRs, monitors logs,
and counts evidence. No mutation surface beyond `kubectl apply -f` of
the four sandbox CRs.

### 1b. `file_read` sub-agent tool (`agt-task-loop.ts`,
`agt-task-tools.ts`)

Sub-agents can now invoke `file_read(path)` to read a file from their
own sandbox filesystem. The implementation:

- Calls `fs.readFile` server-side in the OpenClaw plugin process
  (UID 1000, same identity as the agent's own shell tool).
- Refuses absolute paths outside the agent's sandbox via a path
  prefix check (`/sandbox/`, `/tmp/`, agent's $HOME).
- Returns bytes back through the existing tool-result envelope.

This is **not** a new capability — agents already had `exec_command`
which can run `cat`. `file_read` is a typed wrapper that returns
errors as structured JSON instead of process exit codes. It does not
bypass any policy, does not cross the sandbox boundary, and is
seccomp-bounded.

### 1c. Stability fixes

- Viz/parent prompt fixes — pure prompt copy adjustments.
- KarsSandbox `memoryRef` plumbing — controller-side change covered
  by the existing CRD schema; no capability change.
- Watchdog timeout bump — sub-agent watchdog gives sibling artefacts
  more time to arrive over the mesh. No capability change.

### 1d. Docs

- New "What you can see while it runs (Headlamp + Grafana)" section
  in `docs/use-cases/exec-brief-walkthrough.md`.
- Blockquote callout from `docs/security.md` to the walkthrough's
  per-layer-proof section.

Pure documentation. Not in capability paths.

## 2. Capability Surface

| Capability | Pre-change | Post-change |
|---|---|---|
| Sub-agent filesystem read | `exec_command` + `cat` | Same — new `file_read` is a typed wrapper using identical underlying syscalls |
| Sub-agent prompt | covered in #331 audit | Pure copy tweaks only |
| `KarsSandbox.memoryRef` | controller resolver already trusted | Same |

No new capabilities.

## 3. Crypto Surface

No change. See #331 audit.

## 4. Secrets Handling

No change. See #331 audit. `file_read` cannot read Kubernetes
secrets — the sandbox container has no Secret volume mounts beyond
the per-sandbox `<name>-credentials` Secret (when present), and that
Secret is mounted as `envFrom`, not as files.

## 5. Test Coverage

- local-k8s exec-brief: **9/9 PASS** on run `20260525T205529Z` (full
  harness PASS — same scenario set as the AKS run).
- AKS exec-brief: still 9/9 PASS (no regression — same harness API
  and scenario files).
- Controller: 770/770. Router: 105/105. CLI: 769/769.

## 6. Network / NetworkPolicy review

No NetworkPolicy changes. The harness applies `KarsSandbox` CRs that
the controller renders into the existing NetworkPolicy template.

## 7. Sign-offs

Signed-off-by: Pal Lakatos <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
