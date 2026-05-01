# Dev-mode `/tmp/openclaw-stage` Permissions Fix + Mermaid Diagram Repairs

- **Date:** 2026-04-30
- **Slice:** post-Phase 2 polish
- **Author:** Phase 2 train

## Scope

End-to-end test of `azureclaw dev` against a fresh local sandbox surfaced
two unrelated regressions:

1. The OpenClaw gateway and Node host crashed at startup with `EACCES` on
   `/tmp/openclaw-stage/openclaw-2026.4.27-<hash>/.openclaw-runtime-deps.lock`,
   leaving the sandbox alive but with no agent process.
2. `docs/architecture-diagrams.md` had two diagrams that failed to parse
   under `mermaid` 11.x — section §6.1 (Inference Router Data Path) and
   §14.2 (PolicyEnvelope Hot-Reload State Machine).

This change addresses both.

## Fix 1 — `sandbox-images/openclaw/entrypoint.sh` multi-root resolver

### Root cause

`/opt/openclaw-stage` is built into the image at build time with mode
`a+rX` (Dockerfile.base line 101). The previous entrypoint copied it to
the writable `/tmp` tmpfs because OpenClaw 2026.4.x writes a
`.openclaw-runtime-deps.lock` sentinel inside the version-hash dir on
first plugin-runtime resolve, and the rootfs is read-only:

```sh
cp -r /opt/openclaw-stage /tmp/openclaw-stage
chmod -R u+w /tmp/openclaw-stage
```

This had two compounding problems:

1. **In dev mode** the entrypoint runs as root, `cp -r` produced a
   root-owned tree, and `chmod u+w` only added write for root. When the
   entrypoint later switched to `runuser -u sandbox`, sandbox could not
   write the lock sentinel — gateway and Node host crashed with `EACCES`.
2. **Independent of the chown bug**, the `cp -r` materializes the
   *entire* staged tree into tmpfs every container boot — ~600 MiB
   today, growing with each OpenClaw release. After Fix 3 (gap-fill of
   missing deps) brought the stage to ~1.8 GiB, the copy started failing
   with `No space left on device` against the 1 GiB `/tmp` tmpfs.
3. **Posture compromise**: the `chmod -R u+w` made every bundled JS
   file writable by the agent UID — bundled TCB became mutable at runtime.

### Fix

OpenClaw's bundled-runtime-deps resolver supports a multi-root design
that the previous entrypoint wasn't using. Verified against
`dist/bundled-runtime-root-D11Fl_T4.js` in OpenClaw 2026.4.27:

- `OPENCLAW_PLUGIN_STAGE_DIR` is colon-separated (line ~666); all
  entries become NODE_PATH search roots (line ~1743).
- `missingSpecs = deps.filter(dep => !hasDependencySentinel(searchRoots, dep))`
  (line ~1455). With every dep present in *any* search root,
  `missingSpecs = []` and the install path (lock dir, npm spawn,
  retained-manifest write) **never executes**.
- The *last* entry is the install root, used only when something is
  actually missing (line ~1090: `externalRoots.at(-1)`).

New entrypoint:

```sh
if [ -z "${OPENCLAW_PLUGIN_STAGE_DIR:-}" ] && [ -d /opt/openclaw-stage ]; then
  mkdir -p /tmp/openclaw-cache 2>/dev/null || true
  if [ "$(id -u)" = "0" ]; then
    chown sandbox:sandbox /tmp/openclaw-cache 2>/dev/null || true
  fi
  export OPENCLAW_PLUGIN_STAGE_DIR=/opt/openclaw-stage:/tmp/openclaw-cache
fi
```

Verified at runtime as UID 1000 with `/opt` mounted read-only:
all 68 bundled plugins load, 0 errors, `/tmp/openclaw-cache` peaks
at ~27 MiB (NODE_PATH symlinks back into `/opt`, plus a few small
manifests). No `cp -r`, no chmod-u+w on bundled code, no lock dir
ever created.

### Threat-model analysis

| Concern | Outcome |
|---|---|
| Agent UID can tamper with bundled code? | **No.** `/opt/openclaw-stage` is mounted read-only at runtime (rootfs is read-only). The agent UID has read+execute via `a+rX` baked at build time, and **no write path** — the previous `chmod -R u+w` is gone. Bundled TCB is now genuinely immutable at runtime. |
| Cache dir poisoning between containers? | **No.** `/tmp` is tmpfs, scoped per-container, wiped on every restart. Each container starts with an empty cache that OpenClaw populates from the read-only stage on first plugin load. |
| Cross-UID write to cache? | **No.** `chown sandbox:sandbox /tmp/openclaw-cache` runs only in the dev path (`id -u == 0`). In AKS the entrypoint already starts as sandbox so the `mkdir` produces sandbox-owned dirs. Router UID (1001) has no path that writes here. |
| What if a dep is genuinely missing at runtime (regression)? | **Fail closed.** With egress-guard blocking npm registry on UID 1000, `installBundledRuntimeDepsAsync` would attempt a 403-prone `npm install` and the affected plugin would fail to load. Fix 3's build-time assertion catches this before the image ships, so the runtime path is exercised only for unanticipated regressions. |
| AKS impact? | **Improved.** Pod `tmp` `emptyDir.sizeLimit` stays at `1Gi` (no bump). Production memory footprint unchanged. The AKS path no longer relies on the `cp -r` working — same multi-root resolver, same ~27 MiB cache. |

### Verification

```
$ docker run --rm --user 1000:1000 \
    -e OPENCLAW_PLUGIN_STAGE_DIR=/opt/openclaw-stage:/tmp/openclaw-cache \
    azureclaw-sandbox-base:dev sh -c '
      mkdir -p /tmp/openclaw-cache
      openclaw doctor 2>&1 | tail -5
      du -sh /tmp/openclaw-cache
      find /tmp/openclaw-cache -name ".openclaw-runtime-deps.lock"
    '

  Plugins:  Loaded: 68   Errors: 0
  27M      /tmp/openclaw-cache
  (no lock dir created)

$ touch /opt/openclaw-stage/probe   # writes correctly denied
  touch: cannot touch '/opt/openclaw-stage/probe': Permission denied
```

## Fix 2 — `docs/architecture-diagrams.md` mermaid parse errors

Two diagrams failed `mermaid-cli` 11.14 parsing.

### §6.1 — sequenceDiagram

```
Note right of CS: ❌ 400 if threshold breached<br/>(always-on; InferencePolicy can tighten)
                                                              ^
```

Mermaid sequenceDiagrams treat `;` as an alternative line separator. The
note text was truncated mid-parenthetical.

**Fix:** replace `;` with em-dash inside the note body. Pure text change,
no semantic shift.

### §14.2 — stateDiagram-v2

```
Empty --> Loaded: PolicyChange::Upserted\n(first policy)
                              ^
```

Mermaid stateDiagram-v2 splits transition labels on `:`. The
Rust-style `::` enum-path syntax in the label confused the parser.

**Fix:** replace `::` with `.` (`PolicyChange.Upserted`) inside the
diagram labels only. Five lines updated. No source code or doc text
outside the diagram is affected. Surrounding prose still uses the Rust
`PolicyChange::Upserted` syntax.

### Verification

Lint-passed all 30 mermaid blocks in `docs/architecture-diagrams.md`
locally with `npx -y -p @mermaid-js/mermaid-cli mmdc`. Zero parse
errors.

## Fix 3 — `Dockerfile.base` extension-deps gap-fill

### Root cause

After Fix 1 unblocked the gateway from staring, end-to-end testing surfaced
a second pre-existing issue: the node-host (`openclaw node`, started at
entrypoint.sh:978) crashed at startup with
`Cannot find module '@mariozechner/pi-ai/dist/oauth.js'`. Without the
node-host, the gateway hosts `agents.list` and accepts WS connections but
no agent turn ever runs — chat sends silently stall.

The missing module is declared as a dependency in the bundled `xai`
extension's `package.json`. OpenClaw's own bundled chunks (e.g.
`dist/oauth-*.js`, loaded by `memory-core` in the node-host) statically
require it whether or not `xai` is enabled. `openclaw doctor --fix` at
base-image build time only stages deps for *configured* plugins, so
`pi-ai` was being skipped — leaving a latent crash that was previously
masked by Fix 1's EACCES.

### Fix

Add `sandbox-images/openclaw/stage-extension-deps.sh` (build-time helper)
and invoke it from `Dockerfile.base` immediately after `openclaw doctor`.
The helper:

1. Walks every bundled extension's `package.json`
2. Takes the union of declared `dependencies`
3. Installs anything not already resolvable in the stage tree's
   `node_modules` via `npm install --no-save --omit=dev`

Then a small assertion fails the build if
`@mariozechner/pi-ai/dist/oauth.js` is still missing — converting future
regressions into build failures rather than silent runtime breakage.

### Threat-model analysis

| Concern | Outcome |
|---|---|
| New network access at build time? | **No.** The base image already runs `openclaw doctor --fix` and `npm audit fix --force` and several `openclaw skills install` commands. Threat model unchanged: full network at build, frozen-in-image at runtime. |
| New runtime privileges? | **No.** Helper runs at build time only. Runtime stage tree is under `OPENCLAW_PLUGIN_STAGE_DIR=/opt/openclaw-stage:/tmp/openclaw-cache`; `/opt` is read-only at runtime (Fix 1) and `/tmp/openclaw-cache` is sandbox-owned tmpfs scoped per-container. |
| Extra packages = larger TCB? | **Larger image, not larger runtime memory.** Union over every bundled extension's manifest brings in heavyweights (e.g. `@openai/codex-linux-arm64`); staged tree grows from ~600 MiB to ~1.8 GiB **on disk in the image**. At runtime, OpenClaw resolves directly from `/opt/openclaw-stage` via NODE_PATH (Fix 1) — nothing is copied, only the ~27 MiB symlink-cache lives in tmpfs. The 1.8 GiB sits cold on the read-only rootfs. |
| Why not narrow to an allowlist (`pi-ai` only)? | The union approach guarantees that *every* manifest-declared dep is pre-staged and frozen-in-image. Future bundle changes can introduce new transitive `require()`s without warning; the broad pre-stage prevents runtime npm-install fallback (which fails closed under egress-guard). The build-time assertion + multi-root resolver means the only cost is image size on the read-only rootfs. |
| Supply-chain pinning? | **Same as upstream.** Versions come from each extension manifest's `dependencies` (committed in OpenClaw's own bundle). We don't introduce new version ranges. |

### Verification

- Without the fix: container starts, gateway "ready", node-host crashes,
  WebUI connects, chat sends silently stall.
- With the fix (build-time assertion passes, runtime smoke):
  `docker logs azureclaw-dev-agent | grep -i "Failed to start CLI"` is
  empty; `tail /tmp/node-host.log` shows clean plugin registration.

## Design history (what we tried, what we shipped)

Initial diagnosis pointed at the dev-mode `chown` gap (root-owned
staged tree). Fixing that unblocked layer 1 (gateway start) and exposed
layer 2 (`pi-ai/dist/oauth.js` missing → node-host crash → silent chat
stall). The first cut at Fix 3 used a build-time union helper, which
grew the stage to ~1.8 GiB and broke the entrypoint's `cp -r` against
the 1 GiB `/tmp` tmpfs.

We considered four alternatives before landing on the multi-root
resolver:

| Option | Approach | Cost / blocker |
|---|---|---|
| A | Pre-create the `.openclaw-runtime-deps.lock` sentinel at build time | Lock dir is created on-demand only when an install is triggered; pre-creating it would deadlock the resolver (line ~309 fails with `EEXIST`, falls into stale-detection retry loop). |
| B | overlayfs mount with `/opt/openclaw-stage` lowerdir + tmpfs upperdir | Requires `mount` syscall in entrypoint; doable but adds a CAP_SYS_ADMIN-equivalent step. Defeated by simpler search-roots design. |
| C | Symlink farm (`cp -rs`) | Cleaner than full copy but still leaves a writable tree path; the multi-root design eliminates the writable-tree requirement entirely. |
| D | Bump `/tmp` tmpfs `1Gi → 3Gi` (dev + AKS) | Adds +2 GiB memory cgroup pressure per pod in AKS; preserves the writable-stage compromise. Considered, prototyped, **reverted** in favor of Fix 1's multi-root resolver. |
| **E (shipped)** | **Multi-root `OPENCLAW_PLUGIN_STAGE_DIR` (read-only stage + writable tmpfs cache)** | **Zero compromise: stage stays read-only, cache is ~27 MiB scratch, no tmpfs bump, no mount syscall, no runtime npm trust.** |

The journey is documented for future maintainers who hit similar gaps
with OpenClaw's plugin-runtime resolver — when in doubt, **check the
search-roots / install-root distinction in `bundled-runtime-root-*.js`**
before assuming a writable stage tree is required.

## CI gate considerations

`ci/security-audit-required.sh` flags `sandbox-images/openclaw/entrypoint.sh`,
`sandbox-images/openclaw/Dockerfile.base`, and `controller/src/reconciler/mod.rs`
as capability-introducing paths. This audit document discharges the gate
for this PR.

## Fix 4 — Mesh-send payload guard + `mesh_transfer_file` sub-agent tool + `telegram_status` parent tool

### Root cause

Three demo-day failure modes surfaced in the multimedia-brief run on
`dev`:

1. **Cross-container path leakage.** `viz` produced a chart on
   foundry_code_execute and tried to ship it to `writer` by sending
   `mesh_send(to=writer, content='{"type":"file_transfer", "file_path":"/sandbox/chart.png"}')`.
   The peer agent runs in its own container and cannot read
   `viz`'s `/sandbox/`. Result: writer received a JSON envelope with
   no actual bytes and embedded the literal string
   `data:image/png;base64,<base64-image-data>` into the brief. The
   sub-agent tool surface (`agt-task-tools.ts`) had no
   `mesh_transfer_file` peer of the parent's `azureclaw_mesh_transfer_file`,
   so the LLM was forced to construct `file_transfer` JSON by hand.

2. **Placeholder bytes in `file_data`.** Other times the LLM built
   `{"type":"file_transfer", "file_data":"<base64-image-data>"}` —
   a literal angle-bracketed placeholder rather than real base64.
   The recipient gateway dutifully tried to decode and got garbage.

3. **Telegram channel routing.** The user prompt referenced "the
   telegram.dev channel"; the LLM passed `target:"telegram.dev"` to
   OpenClaw's built-in `message` tool, which rejected with
   `Unknown target "telegram.dev" for Telegram. Hint: <chatId>`.
   Half the planned status thread never went out. Parent has Telegram
   wired (env vars `TELEGRAM_BOT_TOKEN` + `TELEGRAM_ALLOW_FROM`) but
   the LLM had no tool that took only `text` — every code path
   required figuring out the channel routing or chat-ID layer.

### Threat model

- **Same-pod data exfiltration:** the new `mesh_transfer_file`
  sub-agent tool inherits the parent's TOCTOU-safe open/fstat/read
  pattern, the `/sandbox` confinement check, and the 30 MB max-size
  cap. It does not widen the per-sandbox FS attack surface.
- **Mesh peer impersonation:** unchanged — the wire path remains
  AGT SDK `meshClient.send(targetAmid, ...)` end-to-end, with the
  same retry/backoff semantics already audited in prior slices. The
  new tool is a producer of the same `{type:"file_transfer", ...}`
  envelope the gateway already accepts.
- **Token / secret leakage in error paths:** `telegram_status`
  redacts `TELEGRAM_BOT_TOKEN` from any echoed Bot API error body
  before returning, and from any `fetch()` exception message. Token
  never leaves the process when delivery fails.
- **Payload-guard false positives:** the guard parses content as
  JSON and only inspects `type === "file_transfer"` envelopes
  AND objects whose keys are in a closed set of artifact-path-style
  fields (`file_path`, `artifact_path`, `hero_image_path`,
  `image_path`, `chart_path`, `path`) AND values pointing into
  local container roots (`/sandbox/`, `/tmp/`, `/mnt/data/`,
  `/workspace/`). Free-form prose mentioning a path is not parsed
  as JSON and is allowed through. Plain JSON metadata without the
  closed-set keys is allowed through. Tested in
  `runtimes/openclaw/src/index.test.ts`.
- **Replay / forge:** a hostile sub-agent could already construct
  any `mesh_send` content. The guard is a *self*-protective rail
  that catches the LLM's own errors before they hit the wire — it is
  not a security boundary against an adversarial sub-agent (the
  AGT SDK's signed envelopes remain the boundary).

### Files touched

| File | Change |
|---|---|
| `runtimes/openclaw/src/core/mesh-payload-guard.ts` | **NEW** — pure validator; placeholder/missing/local-path detection. |
| `runtimes/openclaw/src/core/agt-task-tools.ts` | mesh_send description warns about hand-crafted file_transfer; add `mesh_transfer_file` descriptor; mesh_inbox `mark_read` default doc fixed. |
| `runtimes/openclaw/src/core/agt-task-loop.ts` | Wire guard into sub-agent `mesh_send`; add `mesh_transfer_file` handler (TOCTOU-safe open/fstat/read, 30 MB cap, /sandbox confinement); decode `file_transfer` inbox entries (gateway-saved + inline forms); update sub-agent system prompt with `mesh_transfer_file` GOOD example replacing the `<base64-bytes>` placeholder. |
| `runtimes/openclaw/src/core/agt-tools/agt.ts` | Wire guard into parent `azureclaw_mesh_send`; surface `saved_to` + `file_name` + `description` in parent inbox decoder when gateway has rewritten the entry; **register new `telegram_status` tool** with token-redacted error paths. |
| `runtimes/openclaw/src/index.test.ts` | +12 tests covering placeholder rejection, missing file_data rejection, /sandbox-path rejection, plain-text false-positive prevention, plain-JSON metadata allow-through, valid base64 allow-through, telegram_status registration, missing-config error, empty-text error, chat-ID routing from `TELEGRAM_ALLOW_FROM`, token redaction in error responses, multi-chat-ID fan-out. |

### Verification

```bash
cd runtimes/openclaw && npx tsc --noEmit && npm test
# 100 → 112 tests pass

cd cli && npx tsc --noEmit && npm test
# 451 tests pass
```

### Why a new tool instead of relying on the guard alone

The rubber-duck pass surfaced this directly: the previous sub-agent
system prompt contained `mesh_send(...,{type:'file_transfer',
file_data:'<base64-bytes>'})` as a `✅ GOOD` example. Adding a
guard but leaving the prompt unchanged would teach the LLM to send
the rejected pattern, get the error, and try variations. A
dedicated `mesh_transfer_file` tool that hides the encoding ceremony
(open/read/base64/envelope construction) is the correct ergonomic
fix; the guard is the safety net for when the LLM still tries the
old pattern. Both ship in the same commit.

### Telegram tool design notes

- Bypasses OpenClaw's channel routing entirely — the LLM only has
  to know the tool name and the message text.
- Reuses the same Bot API direct-fetch convention already used in
  `azureclaw_handoff_request` (agt-tools/agt.ts:1156-1167) so
  parent-side network behavior is unchanged.
- Multiple chat IDs (`TELEGRAM_ALLOW_FROM=111,222,333`) are
  fanned out and a per-chat result is returned, allowing the LLM
  to retry only failed targets.
- Truncates at 4096 chars (Telegram message ceiling).
- Token sanitized in any error string before it leaves the tool.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
