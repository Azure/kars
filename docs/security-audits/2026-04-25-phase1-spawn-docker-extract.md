# Security audit — Phase 1 spawn::docker submodule extraction

**Capability:** `inference_router::spawn::docker` — extraction of the
Docker dev-mode sandbox spawn path (`azureclaw dev`) from
`inference-router/src/spawn.rs` into a new submodule. Phase 1
hotspot decomposition (plan §4.2 / §7 item 8). The K8s controller-
managed path stays in `spawn::mod`.

**Branch:** `phase1/spawn-docker-extract`
**Date:** 2026-04-25

## 1. Summary

Pure refactor — no behaviour change. `inference-router/src/spawn.rs`
is converted to a module `inference-router/src/spawn/{mod.rs,
docker.rs}`. Every Docker Engine API call used by the dev-mode
spawn path moves to `docker.rs`:

- `collect_sub_agent_snapshots_docker`
- `docker_create_body`
- `docker_api`
- `create_sandbox_docker`
- `get_sandbox_status_docker`
- `list_sandboxes_docker`
- `delete_sandbox_docker`

Internal callers in `spawn::mod` use `docker::*`. External callers in
`crate::routes::handoff` continue to reach
`spawn::list_sandboxes_docker` / `spawn::delete_sandbox_docker`
through pub-use re-export — no caller code changes.

Sizes:
- `spawn/mod.rs` 762 LOC (was `spawn.rs` 1199; −437 LOC; under Phase 1
  cap of 900 by 138 LOC).
- `spawn/docker.rs` 452 LOC (well under the 800 new-file cap).

LOC budget updated: the file path key changed from
`inference-router/src/spawn.rs` to
`inference-router/src/spawn/mod.rs`; baseline + phase caps preserved.

## 2. Threat model delta

None. The Docker path was already shipped, was already gated by the
same `RUNTIME=docker` env-var check at the top of each spawn entry
point, and was already used only by `azureclaw dev`. Moving the code
into a submodule does not change which environments reach it, what
labels it stamps on containers (`azureclaw.parent=<parent>`), what
network it joins, or what credentials it can see.

## 3. OWASP mapping

- **OWASP MCP Top 10 — A03 (Insecure Process Execution):** the dev-
  mode spawn always runs against a local Docker socket. The check
  remains gated by `RUNTIME=docker`; production sandboxes go through
  the K8s controller path which has its own admission policies. No
  privilege boundary changed.
- **OWASP LLM Top 10 v2.0 — LLM05 (Improper Output Handling):** the
  Docker filter URL-encoding (lines 703-710 of the new module)
  retains the original mitigation against shell/JSON glob characters
  reaching curl unescaped.

## 4. AuthN / AuthZ path

Unchanged. Both K8s and Docker paths already required the existing
admin-token check via the router's middleware; the spawn module
itself does no auth (it runs after middleware).

## 5. Secret + key custody

Unchanged. The K8s path still uses `propagate_credentials` for the
namespace's credentials Secret. The Docker path never had a
secret-propagation step (dev-only).

## 6. Egress surface delta

Zero. Same `docker_api` Unix-socket calls, same
`/containers/json`, `/networks/create`, `/containers/create`,
`/containers/<id>/start`, `/containers/<id>/stop`,
`/containers/<id>` paths.

## 7. Audit events emitted

Unchanged. Spawn-level audit events are emitted by `routes::spawn`
upstream; this module is a pure execution path.

## 8. Failure mode

**Fail-closed.** Every Docker API call still returns `Err(String)` on
non-2xx; no path coerces an error into a fake success.

## 9. Negative-test coverage

585 lib tests pass pre+post. Existing `spawn::tests::*` covers the
serde negative cases (`handoff_meta_rejects_unknown_fields` and
others). The Docker path remains exercised by manual `azureclaw dev`
runs (full integration not part of `cargo test`).

## 10. Vendored / third-party dependency delta

None. Same `kube`, `serde`, `tokio` usage.

## 11. Sign-offs

Phase 1 hotspot decomposition pass #2 (plan §4.2). spawn brought
under Phase 1 cap (1199 → 762, target was 900). All 6 CI gates green;
clippy clean.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
