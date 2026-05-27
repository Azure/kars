# Security Audit — kars lowercase + Agent Reference Stack long form (rebrand polish)

**Scope**: PR #358 — `chore/rebrand-lowercase-and-longform`. Branding follow-up to PR #355 (the hard `azureclaw → kars` rebrand). Two changes:

1. Lowercase `Kars` → `kars` in prose (743 substitutions); code identifiers `KarsSandbox`/`KarsMemory`/`KarsEval`/`KarsPairing` preserved by regex word-class guard.
2. README headline and TRADEMARKS.md rewritten to use the formal name "Agent Reference Stack for Kubernetes" with `(kars)` short form on first reference.

The gate triggers on touched capability-introducing files in `cli/src/commands/`, `controller/src/`, `inference-router/src/`, `runtimes/openclaw/src/`, and `sandbox-images/`. **All edits in this PR are prose-only**: comments, log strings, JSDoc descriptions, Dockerfile labels, entrypoint banners, environment-variable comments. **No control-flow, no security-policy logic, no auth, no crypto, no networking changes.**

## 1. Sanity-check: what kinds of lines did the substitution touch?

The regex `\bKars(?![A-Za-z])` lowercases standalone `Kars` to `kars`. By construction it touches:

- Comments (`// Kars Operator TUI ...` → `// kars Operator TUI ...`)
- Log strings (`log.warn("Kars relay ...")` → `log.warn("kars relay ...")`)
- Error messages (`format!("Kars rejected ...")` → `format!("kars rejected ...")`)
- JSDoc descriptions of CLI commands
- Dockerfile labels (`org.opencontainers.image.title="Kars X"` → `... "kars X"`)
- Entrypoint banner echoes

It does **not** touch:

- `KarsSandbox`, `KarsMemory`, `KarsEval`, `KarsPairing` and any other `Kars` followed by a letter — these are Rust types + Kubernetes CRD kinds and must remain PascalCase per Rust naming conventions and K8s API conventions.
- Module paths, package names, env-var names — those are `kars` lowercase already (from PR #355).

## 2. Capability impact

**None.** No new behaviour added; no behaviour removed; no policy changes. The 765-line diff is purely cosmetic.

## 3. Crypto / Secrets / NetworkPolicy / Streaming

All unchanged.

## 4. Test Coverage

- `cargo test --workspace` — all crates ✅ (1859/1859 baseline preserved)
- `cd cli && npm test` — 769/769 ✅
- Final grep:
  - `rg '\bKars\b'` (standalone, not a code identifier) → 0 occurrences
  - `rg 'azureclaw|AzureClaw|AZURECLAW'` → 0 occurrences

## 5. GitHub repo description

Updated via `gh repo edit --description` to match the README's multi-runtime framing (OpenClaw is now one of seven runtime adapters, not the only one). Pure metadata, no code or CI impact.

## 6. Sign-offs

Signed-off-by: Pal Lakatos <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
