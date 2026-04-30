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

## Fix 1 — `sandbox-images/openclaw/entrypoint.sh` permissions

### Root cause

`/opt/openclaw-stage` is built into the image at build time with mode
`a+rX` (Dockerfile.base line 101). At container start, the entrypoint
copies it to the writable `/tmp` tmpfs:

```sh
cp -r /opt/openclaw-stage /tmp/openclaw-stage
chmod -R u+w /tmp/openclaw-stage
```

In **AKS**, the pod's `securityContext.runAsUser=1000` makes the
entrypoint start as the sandbox user, so `cp -r` produces a
sandbox-owned tree and `chmod u+w` (owner-write) suffices.

In **dev mode** (Docker, no `USER` directive), the entrypoint runs as
root, `cp -r` produces a root-owned tree, and `chmod u+w` only adds
write for root. When the entrypoint later runs OpenClaw via
`runuser -u sandbox --`, sandbox cannot write the
`.openclaw-runtime-deps.lock` sentinel that OpenClaw 2026.4.x creates
on first plugin-runtime resolve — the gateway and Node host both crash
with `EACCES`.

### Fix

After the `cp`, when running as root, chown the staged tree to
`sandbox:sandbox` before the `chmod`. The branch is gated on
`id -u == 0`, so the AKS path is byte-identical to before.

```sh
if [ -z "${OPENCLAW_PLUGIN_STAGE_DIR:-}" ] && [ -d /opt/openclaw-stage ]; then
  if [ ! -d /tmp/openclaw-stage ]; then
    cp -r /opt/openclaw-stage /tmp/openclaw-stage
    if [ "$(id -u)" = "0" ]; then
      chown -R sandbox:sandbox /tmp/openclaw-stage 2>/dev/null || true
    fi
    chmod -R u+w /tmp/openclaw-stage 2>/dev/null || true
  fi
  export OPENCLAW_PLUGIN_STAGE_DIR=/tmp/openclaw-stage
fi
```

### Threat-model analysis

| Concern | Outcome |
|---|---|
| Does this grant a new privilege to the sandbox user? | **No.** The sandbox UID already executes these `node_modules` at runtime. Making the local copy writable by sandbox does not add capability — you cannot escalate by modifying code you already execute. |
| Could a router or other UID exploit this? | **No.** `chown sandbox:sandbox` was chosen over `chmod a+w` deliberately: `a+w` would also let the router UID (1001) write, breaking sandbox/router isolation. After the chown, only sandbox (and root, in dev) can write; router retains the read-execute it had via `a+rX`. |
| Does this affect AKS? | **No.** The new `chown` branch is gated on `id -u == 0`. In AKS, `runAsUser=1000` ⇒ `id -u` is `1000` ⇒ the branch never executes. Production posture is unchanged. |
| Persistence / supply-chain risk? | **No.** `/tmp` is tmpfs, wiped on container restart. The original `/opt/openclaw-stage` on the read-only rootfs is untouched. |
| Affect on plugin hardening? | **No.** The `chown -R root:sandbox $PLUGIN_DIR` step (entrypoint.sh:731) operates on `/sandbox/.openclaw/plugins/`, a separate tree. That is still root-owned, read-only for sandbox. |

### Verification

Reproduced the crash before the fix; confirmed clean start after:

```
Before: ○ OpenClaw gateway (starting...)   — gateway.log: PluginLoadFailureError EACCES
After:  ✓ OpenClaw gateway (ready)
        ✓ Inference router (ready)
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

## CI gate considerations

`ci/security-audit-required.sh` flags `sandbox-images/openclaw/entrypoint.sh`
as a capability-introducing path. This audit document discharges the gate
for this PR.

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
