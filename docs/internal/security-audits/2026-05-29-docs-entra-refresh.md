# Security Audit — Docs Refresh + Brand-Reference Scrub

**Scope**: PR #366 — `docs/entra-refresh`. README + architecture docs
refresh + scrub of three residual pre-rebrand brand references in
tracked source files.

## 1. Capability surface

**Zero new capability.** Every code change in this PR is text-only:

| File | Change | Capability impact |
|------|--------|---|
| `cli/src/commands/up/headlamp_stack.ts` | Comment-only edit (lines 113-116) rephrasing a historical bug-context note in `installKarsPlugin`. No control-flow, no policy, no I/O change. | None — comment text only |
| `docs/agent-identity.md` | Two prose references `azureclaw spawn` → `kars_spawn` | None — doc text only |
| `docs/architecture/entra-agent-id/main.bicep` | Comment in a POC bicep template: namespace label fixed from `azureclaw-<name>` → `kars-<name>` to match what the controller actually emits | None — comment text only (POC template, not deployed by `kars up`) |
| `README.md`, `docs/architecture.md`, `docs/architecture-diagrams.md` | Identity-story refresh: dual-mode `kars up --mesh-trust=anonymous\|entra` instead of the old generic "Workload Identity" label | None — doc text only |

The `cli/src/commands/up/headlamp_stack.ts` edit is on the
capability-path regex of `ci/security-audit-required.sh` (anything
under `cli/src/commands/` is treated as capability-introducing by
default), but the actual change is six characters of comment text.
There is no logic change, no new flag, no new external call.

## 2. Verification

- `git diff origin/main -- cli/src/commands/up/headlamp_stack.ts` shows
  only the comment block change.
- 786/786 CLI tests pass; `loc` / `no-stubs` / `copyright-headers` gates
  all clean locally.

## 3. Reviewer sign-off

This audit exists to satisfy the `security-audit-required` gate's
requirement that any `cli/src/commands/` touch ships with an audit.
The actual change is documentation polish.

---

Signed-off-by: Pal Lakatos-Toth <lakatos.toth.pal@gmail.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
