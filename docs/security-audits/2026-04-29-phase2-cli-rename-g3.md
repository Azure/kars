# Phase 2 — S15.g.3 CLI Rename

**Date:** 2026-04-29
**Branch:** `phase2-cli-rename-g3`
**Slice:** S15.g.3

## Scope

Rename `cli/` npm package from `@azure/azureclaw` to `@azureclaw/cli`, and
clean up three stale `package.json` entries left behind by the S15.g.1 plugin
extraction.

## Rationale

After S15.g.1 + S15.g.2, the workspace package layout is:

| Package | Scope |
|---|---|
| `cli/` | `@azure/azureclaw` ← outlier |
| `runtimes/openclaw/` | `@azureclaw/runtime-openclaw` |
| `mesh-plugin/` | `@azureclaw/mesh` |
| `tests/compat/` | `@azureclaw/tests-compat` |
| `tests/conformance/` | `@azureclaw/tests-conformance` |

Renaming `cli/` to `@azureclaw/cli` makes the scope consistent across the
entire workspace.

While renaming, also removed three stale entries in `cli/package.json` that
became dangling after S15.g.1:

- `main: "dist/plugin.js"`
- `types: "dist/plugin.d.ts"`
- `openclaw.extensions: ["./dist/plugin.js"]`

`cli/src/plugin.ts` was moved to `runtimes/openclaw/src/index.ts` in S15.g.1
and `cli/dist/plugin.js` is no longer emitted by `tsc`. The CLI is a pure
binary package — `bin` is the only needed entry.

## Security considerations

None — package metadata only. No source code, permissions, or runtime
behavior changes.

## Verification

- `cli` build: green.
- `cli` typecheck (`tsc --noEmit`): green.
- `cli` tests: 354 pass / 2 skip.
- `cli` lint: 0 errors (16 pre-existing warnings unchanged).
- `cli/package-lock.json` regenerated and committed.
- `grep -rn "@azure/azureclaw"` outside historical audit docs: 0 matches.

## Files touched

- `cli/package.json`
- `cli/package-lock.json`
- `.github/copilot-instructions.md`
- `CHANGELOG.md`
- `docs/security-audits/2026-04-29-phase2-cli-rename-g3.md` (this file)


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
