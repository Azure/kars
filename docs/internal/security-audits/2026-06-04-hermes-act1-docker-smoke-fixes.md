# Security Audit — Hermes Act 1 Docker Smoke Fixes

**Date:** 2026-06-04
**Branch:** `hermes/act1-docker-smoke-fixes`
**Commit under audit:** 7fb9d68 (`fix(hermes): A1 docker smoke fixes — version pin + plugin opt-in`)
**Reviewers:** Pal Lakatos, Copilot

## Scope

Two files changed:
- `sandbox-images/hermes/Dockerfile` — bump `HERMES_VERSION` 0.5.1 → 0.15.2,
  drop ripgrep from the tdnf install (not available in Azure Linux 3,
  Hermes falls back to grep)
- `sandbox-images/hermes/entrypoint.sh` — emit `plugins.enabled: [kars]`
  in the auto-generated `$HERMES_HOME/config.yaml` so Hermes actually
  loads the kars plugin (was previously discovered + silently skipped
  because Hermes treats standalone plugins as opt-in)

No code-execution paths added. No new privileges granted. No new
network egress. No new file mounts.

## Threat model

### T1: Plugin auto-enable bypasses operator opt-in (CONSIDERED, MITIGATED)

The entrypoint now writes `plugins.enabled: [kars]` automatically on
every boot. Could a malicious user-supplied config disable this by
landing first? No — the entrypoint is the only writer of
`$HERMES_HOME/config.yaml` inside the image. `$HERMES_HOME` is an
emptyDir / tmpfs mount that starts empty on every pod restart and is
not user-writable from outside the container.

The awk merge ALWAYS replaces any prior `plugins:` block — even if
an attacker somehow landed a config.yaml beforehand, the rewrite
would clobber their `plugins.enabled: []` attempt.

### T2: Version bump pulls compromised Hermes release (CONSIDERED, ACCEPTED)

`hermes-agent==0.15.2` is the current latest stable from PyPI
(Nous Research, MIT licence). Same supply-chain trust assumptions as
every other pip-installed dep in the image (croniter, fire, httpx,
jinja2, openai, prompt_toolkit, psutil, pydantic, PyJWT,
python-dotenv, pyyaml, requests, rich, ruamel.yaml, tenacity).

The pin is exact (`==0.15.2`), so a malicious PyPI publish to
`hermes-agent==0.15.3` wouldn't auto-apply without an operator
explicitly bumping the `ARG HERMES_VERSION` default and rebuilding.

Future hardening: add `--require-hashes` with a hash pin for
`hermes-agent` (deferred — same gap exists for every other pip dep
in the image and would be a broader sweep).

### T3: ripgrep removal breaks downstream security tools (CONSIDERED, NO IMPACT)

Hermes' built-in `file_search` tool prefers ripgrep but falls back to
grep when absent. The kars plugin doesn't use ripgrep. No other
sandbox tooling depends on it. Empirically verified: image builds
clean, all 83 unit tests pass, `discover_plugins()` end-to-end works
without ripgrep.

### T4: Plugin code path now actually executes (NEW RISK SURFACE)

Before this fix the kars plugin code was DEAD INSIDE THE CONTAINER —
discovered but never loaded. With this fix the plugin's `register()`
function runs on every Hermes startup, registering 10 tools and
2 hooks (pre_tool_call + post_tool_call).

Net effect: the surface previously audited in
`2026-06-03-hermes-act1-foundation.md`, `2026-06-03-hermes-a1-3-and-a1-4.md`,
`2026-06-03-hermes-a1-5-thru-a1-7.md`, and
`2026-06-03-hermes-a1-10-telemetry.md` now actually becomes live in
the image. Those audits already covered:

- pre_tool_call AGT evaluation (governance.py) — fail-closed grace
- secret redaction in canonicalized action verbs — Bearer/JWT/ghp_/sk-/api_key=
- spawn name DNS-label validation — 63-char cap
- post_tool_call telemetry push — trust + signing-counter to router
- mesh stubs return Act 2 errors — never silently no-op
- foundry_memory uses `memory-<sandbox>` convention — operator-bound

This commit doesn't add any new sensitive code path beyond what those
audits already cover. It only flips the existing audited surface
from dormant to active.

## Verification

- `docker build --platform linux/amd64 -f sandbox-images/hermes/Dockerfile -t kars-sandbox-hermes:dev .` succeeds
- `docker run … python3 -c "from hermes_cli.plugins import discover_plugins, get_plugin_manager; …"` shows:
  - `Plugin 'kars' (source=user, kind=standalone, path=/tmp/hh/plugins/kars)` loaded
  - 10 tools registered (kars_spawn family, kars_discover, http_fetch, 4 mesh stubs)
  - 2 hooks registered (pre_tool_call, post_tool_call)
  - `kars: enabled=True error=None`
- Full entrypoint dry-run (sed-stubbed final `exec hermes`) produces correct config.yaml
- 83/83 Python unit tests still pass inside the image

## Decision

**Approved.**

Two operationally critical bugs fixed; no new threat surface
introduced beyond what the existing A1 audits already cover.

Signed-off-by: Pal Lakatos <pal.lakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
