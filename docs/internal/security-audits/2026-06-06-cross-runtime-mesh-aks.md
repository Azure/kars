# Security Audit — Cross-runtime mesh on AKS (Hermes ↔ OpenClaw)

**Date:** 2026-06-06
**Branch:** `hermes/act1-docker-smoke-fixes`
**Commits under audit:**
- `19ec22c` `fix(mesh-plugin): emit modern did:mesh:<sha256[:32]> DID format`
- `dd810e3` `build(agt): move AGT pin from pallakatos fork to upstream microsoft branch`
- `0764dc8` `fix(agt-mesh-python): JSON-wrap payload to interop with TS SDK receiver`
- `8416c0a` `fix(controller,hermes,agt-mesh-python): operator panel shows mesh peers on Hermes side`
- `2d2131c` `fix(hermes): trust score at OpenClaw-convention baseline + bidi harness`
- `28704be` `fix(hermes): Entra OAuth identity verification for operator-panel parity`
- `496cc92` `feat(aks): foundryRbac auto-grant + Entra-JWT WS connect + AKS test suite`
- Productization slice (this audit): `kars push --only runtime-hermes` auto-builds AGT wheels; `kars up` ships the Hermes adapter image; prekey-clobber flock guard in `kars_agt_mesh.client.connect()`; helm `HERMES_RUNTIME_IMAGE` env wiring; `runtimes.hermes.image` values key.
- Documentation slice (this audit): `docs/hermes-plugin.md`, `docs/runbooks/hermes-troubleshooting.md`, `docs/api/crd-reference.md#hermesconfig`, `docs/runtimes.md` row, `docs/SUMMARY.md` TOC, `docs/getting-started.md` Hermes branch, `docs/channels-plugins.md` Hermes parity note, root `README.md` runtime list.

**Reviewers:** Pal Lakatos, Copilot

---

## Scope

This slice closes the gap between "Hermes Act 1 boots on AKS" and "Hermes is a first-class kars runtime indistinguishable from OpenClaw in the operator UX". The work splits into three categories:

1. **Cross-runtime mesh** — Hermes (Python `kars_agt_mesh`) and OpenClaw (TypeScript `@microsoft/agent-governance-sdk`) speak the same Signal Protocol wire format end-to-end; OC→Hermes and Hermes→OC bidi proven on AKS with `score=509 tier=Known interactions=1`.
2. **Productization** — `kars up`, `kars push`, and `kars dev` ship Hermes without any manual `git clone agent-governance-toolkit`, `bash runtimes/build-agt-wheels.sh`, `docker buildx build sandbox-images/hermes`, `kubectl set env PYTHONPATH=/sandbox/.pyhot`, or any other surgical intervention. A fresh-checkout `kars push --only runtime-hermes` works.
3. **Operator-facing documentation** — Hermes has its own page (`docs/hermes-plugin.md`), CRD reference, runbook, getting-started branch, and SUMMARY TOC entry, matching the existing OpenClaw doc surface for parity.

No new code-execution path was introduced. No new privilege was granted. No new network egress was opened. No new file mount was added.

---

## Threat model

### T1: Stale registry prekey bundle silently corrupts daemon state (MITIGATED — new guard)

**Threat.** A Python process other than the running Hermes daemon imports `kars_runtime_hermes.plugin.mesh` and calls `_get_or_init_client()`. The secondary process's `MeshClient.__new__` returns a fresh instance (per-process singleton), `connect()` generates a fresh `X3DHKeyManager` with brand-new signed pre-keys + OTKs, and `upload_prekeys()` PUTs them to the registry. The daemon's in-memory private keys no longer match the public keys peers fetch — every X3DH-derived shared secret diverges, every inbound AEAD frame fails `InvalidTag`, every `Decrypt failed` log line is silent (no traceback, no exception body).

**Realistic trigger.** An operator running `kubectl exec <pod> -c agent -- python3 -c "from kars_runtime_hermes.plugin import mesh; print(mesh._get_or_init_client()._identity.did)"` to inspect the daemon's DID. This came up four times in the 2026-06-05 debugging session that necessitated this audit.

**Mitigation.** `MeshClient._acquire_prekey_writer_lock()` (runtimes/agt-mesh-python/src/kars_agt_mesh/client.py) takes an exclusive `fcntl.flock` on `<identity_dir>/.mesh-prekeys.lock` before uploading prekeys. A second process trying to start a MeshClient for the same identity raises:

```
MeshTransportError: Another mesh-client process already holds
<HERMES_HOME>/.agt/.mesh-prekeys.lock (pid=<N>). Refusing to start a
second MeshClient for did=did:mesh:<...> — would clobber the running
daemon's prekey bundle and break its ability to decrypt incoming
frames. If you ran `python3 -c 'mesh._get_or_init_client()'` from
`kubectl exec` for debugging, that is the trap this guard protects
against — query the daemon via the gateway HTTP API or `kubectl logs`
instead.
```

The error message both names the holder PID (so `kill -0 <pid>` confirms a live owner) and the recommended remediation (gateway HTTP API, `kubectl logs`).

**Test coverage.** `runtimes/agt-mesh-python/tests/test_prekey_writer_lock.py` (4 tests, all green on Python 3.14 macOS):
- `test_lock_file_recorded_with_holder_pid`
- `test_second_process_fails_loud`
- `test_lock_released_on_disconnect`
- `test_connect_propagates_loud_failure` (end-to-end with a real `connect()` call against the lock held by a sibling fd)

The guard is **Linux-only** (uses `fcntl`); Windows logs a single warning and continues — the scenarios this protects against don't occur on Windows pods.

### T2: `kars up` pulls a Hermes adapter image that doesn't exist (MITIGATED — `kars up` now includes Hermes)

**Threat.** Before this slice, `kars up` and `kars up --build` shipped six multi-runtime adapter images (openai-agents, maf-python, anthropic, langgraph, langgraph-ts, pydantic-ai) but NOT `kars-runtime-hermes`. A user who ran `kars up` then `kubectl apply` of a `KarsSandbox kind: Hermes` would see `ImagePullBackOff` because the controller's default `karsacr.azurecr.io/kars-runtime-hermes:latest` was never pushed to their ACR.

**Mitigation.** `cli/src/commands/up.ts` now includes Hermes in both the build-and-push loop and the import-from-source-ACR loop. The helm chart wires `HERMES_RUNTIME_IMAGE` from `runtimes.hermes.image` so the controller reads the right override. The `--skip-runtime-images` CLI flag description was bumped from "6 multi-runtime adapter images" to "7" to match.

**No supply-chain risk.** The Hermes adapter image is built from `sandbox-images/hermes/Dockerfile` with the same image hygiene checks (cargo-deny, npm audit, syft SBOM-eligible) as every other multi-runtime adapter. The Hermes Agent base layer pins to a specific version (`HERMES_VERSION` build arg; default `0.15.2`) from the official Nous Research PyPI distribution.

### T3: `kars push --only runtime-hermes` fails on fresh checkout because `runtimes/wheels/*.whl` is missing (MITIGATED — auto-builds wheels)

**Threat.** Six Python runtime Dockerfiles (anthropic, hermes, langgraph, maf-python, openai-agents, pydantic-ai) `COPY runtimes/wheels/` into their build context. `runtimes/wheels/` is gitignored. The only producer is `runtimes/build-agt-wheels.sh`, which expects `AGT_PYTHON_DIR=~/Private/Repos/agt/agent-governance-toolkit/agent-governance-python` (the original author's path). Anyone else's `kars push --only runtime-hermes` failed with `COPY failed: stat runtimes/wheels: directory not found`.

**Mitigation.** `cli/src/lib/agt-bootstrap.ts` gained `ensureAgtWheels(agtRepo, repoRoot)` — invokes `runtimes/build-agt-wheels.sh` against the AGT clone returned by `ensureAgtRepo()`. Cached via `runtimes/wheels/.agt-sha` (the current `vendor/agt/pin.json` SHA), so subsequent runs are no-ops. Wired into `kars push` (before each Python-runtime build), `kars up` (before the multi-runtime build loop), and `kars dev` (when `--mesh-provider=agt --build`).

**No new trust surface.** The wheels are built from the exact same upstream-microsoft commit (`vendor/agt/pin.json::sha` = `3322175d88baf61e8ceab8e392e29fa2bf9b580a` on `kars-sdk-pop-signing` branch) that the TS SDK tarball and the Rust crate `[patch.crates-io]` block already track. There's one source-of-truth SHA across all three language ecosystems; the wheels are not a new dependency.

### T4: `HERMES_RUNTIME_IMAGE` rename breaks rolling upgrades (MITIGATED — legacy alias accepted)

**Threat.** The Hermes adapter previously read `KARS_HERMES_IMAGE`, an inconsistent naming vs the other six runtimes' `*_RUNTIME_IMAGE` convention. Renaming to `HERMES_RUNTIME_IMAGE` would break an upgrade scenario where the helm chart sets the new env var but the deployed controller pod (older image) still reads the old one.

**Mitigation.** `controller/src/reconciler/runtime.rs::hermes_default_image()` accepts BOTH `HERMES_RUNTIME_IMAGE` (preferred) and `KARS_HERMES_IMAGE` (legacy). Helm template sets only the new name. Unit test `hermes_default_image_honours_legacy_alias_for_one_release` pins the back-compat behaviour. The legacy alias should be removed in the release immediately after `kars 0.5.3`.

### T5: Cross-runtime X3DH AAD mismatch (MITIGATED upstream + verified)

**Threat.** The TS SDK and Python SDK could derive different X3DH shared secrets if the KDF (HKDF inputs), the DH-output order in `dhConcat`, the AAD composition (`IK_initiator || IK_responder`), or the header serialization (`dh + pn(u32 BE) + n(u32 BE)`) drifted between languages. AEAD `InvalidTag` would result on every cross-runtime frame.

**Mitigation.** Both implementations are pinned to the same upstream-microsoft commit (`vendor/agt/pin.json`). I manually verified byte-for-byte parity across:
- `serializeHeader` (TS `package/dist/encryption/ratchet.js` :68-75) vs `MessageHeader.serialize` (Python `agentmesh/encryption/ratchet.py` :41-46): both emit `dhPublicKey(32) || pcl(u32 BE) || mn(u32 BE)` = 40 bytes.
- AAD composition: both compose `caller_ad || x3dh_ad`, where `x3dh_ad = IK_initiator || IK_responder` (32+32 = 64 bytes). Caller AAD on both sides is the byte-encoded string `${sender_did}|${receiver_did}` (the format both TS `MeshClient::establishSession` and Python `MeshClient::_handle_knock_frame` independently emit).
- HKDF inputs: both KDFs use `HKDF(salt=zero[32], info="AgentMesh_X3DH_v1", IKM=F||dhConcat, len=32)` where F is `0xFF × 32`. The previous Python implementation swapped salt/IKM (kept as a regression test in `vendor/agt/MIGRATION-NOTES.md`); the upstream fix lives on `kars-sdk-pop-signing`.

End-to-end proof on AKS production sandboxes (`aks-mesh-peer-openclaw → aks-hermes-bidi`): 0 `Decrypt failed` log entries, Hermes `/agt/trust` populated with `aks-mesh-peer-openclaw: score=509`.

### T6: Documentation gap exposes operator to misconfiguration (MITIGATED — full doc surface)

**Threat.** Prior to this slice, Hermes was nowhere in `docs/`. A user landing on `docs/runtimes.md`, `docs/getting-started.md`, or the root `README.md` would see no mention of Hermes and assume it wasn't supported — or, worse, would try `kars add --runtime Hermes` without knowing about the foundryRbac requirement, the Entra Verified ID expectation, the channel/plugin auto-config flow, or the prekey-clobber guard.

**Mitigation.** Eight new or modified docs cover the entire operator surface. See the "Documentation slice" in the commits-under-audit list above. The runbook (`docs/runbooks/hermes-troubleshooting.md`) specifically calls out the five most common operator-facing failure modes with concrete `kubectl` recipes for each.

---

## Validation

- `cargo test --package kars-controller -- hermes` — 9/9 pass (includes new `hermes_default_image_honours_legacy_alias_for_one_release`).
- `PYTHONPATH=src python -m pytest runtimes/agt-mesh-python/tests/ -v` — 27/27 pass (4 new prekey-writer-lock tests).
- `cli/$ npm run typecheck` — clean.
- AKS bidi proof at 2026-06-05T23:04:56Z — `aks-mesh-peer-openclaw → aks-hermes-bidi`, `score=509 tier=Known interactions=1`, zero `Decrypt failed` log entries.
- `bash tests/e2e/interop/aks_full_suite.sh` — 4/4 PASS.
- `bash tests/e2e/interop/hermes_openclaw_bidi.sh` (kind) — PASS, `score=510 status=delivered_and_replied`.

---

## Out of scope

- Filing the upstream PR against `microsoft/agent-governance-toolkit` carrying the TS-SDK X3DH KDF and connect-frame POP fixes. Tracked as a follow-up.
- Dropping the `KARS_HERMES_IMAGE` legacy alias — kept for one release cycle for back-compat.
- Hermes-as-A2A-ingress — Hermes today is a mesh peer + channel-fronted agent; exposing it via the public `A2AAgent` ingress surface is deferred.

---

## Sign-offs

Signed-off-by: Pal Lakatos <palakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
