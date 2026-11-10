<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Security Audit — Offline package replacement on OverlayFS (beta)

Date: 2026-11-10
Scope: `sandbox-images/openclaw/update-node-dependencies.cjs`, `runtimes/tests/test_openclaw_node_security.py`, `runtimes/openclaw/README.md`.
Gated paths: None under the existing capability-introducing path policy. No policy changes or extension of the previous source-bound exception.

## Summary

Both native sandbox-base builds in release run 42777543505 failed because OverlayFS rejected renaming an existing lower-layer MCP SDK directory with `EXDEV`. Keep the normal rename path; only for `EXDEV`, complete a local rollback copy before removing the original and installing the prepared replacement. Preserve relative links and the package-directory mode in that backup. Restore the backup if installation fails; refuse other rename errors without fallback.

This maintainer-approved repair is bound to integration source `ab78d11d66b626b4a176603643011ebc1cca1dff` and these SHA256 content digests:

| Path | SHA256 |
| --- | --- |
| `sandbox-images/openclaw/update-node-dependencies.cjs` | `26c9e1f326b77531ceee678ad98d5604662969ceac9c47ea8ee08a1c342c2b10` |
| `runtimes/tests/test_openclaw_node_security.py` | `b1140e4ecb1f6587ceeef17a10ea949d625ef823f9cc3395c30d2206231825dd` |
| `runtimes/openclaw/README.md` | `43724df81f1ceeb025c3ebad67b6353fcca54b02ededd6d24ba7a19b8142b96d` |

## T1: New capability / attack surface? (YES/NO)
- NO. This is a build-only filesystem compatibility repair, not an agent capability or runtime installation path.

## T2: Security-control change? (YES/NO/NEUTRAL)
- NEUTRAL. Archive checksums, package identities, engines, dependency/peer constraints, target-path checks and prevalidation are unchanged. No network or lifecycle scripts are enabled. AL3 packaging, pinned runtime versions and installed security updates remain required.
- The existing two-reviewer capability policy and the prior fixed-source exception are unchanged. The user explicitly approved this scoped repair; no second human review is claimed.

## T3: Availability / fail-open risk? (REDUCED/NEUTRAL/INCREASED)
- REDUCED. The observed OverlayFS failure gains a narrow fallback. Incomplete backup copies are removed without modifying the original package. Other errors remain fatal; a failed Docker build step does not produce the final image layer.

## Verification
- `PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest runtimes.tests.test_openclaw_node_security -v` — passed, 28 tests, including replacement, rollback, backup failure and non-EXDEV rejection.
- The three new EXDEV regression cases fail against the exact integrated updater; the unrelated-error guard passes against it.
- `node --check sandbox-images/openclaw/update-node-dependencies.cjs` — passed.
- `git diff --check -- sandbox-images/openclaw/update-node-dependencies.cjs runtimes/tests/test_openclaw_node_security.py runtimes/openclaw/README.md` — passed.
- Initial rollback-mode test failure was retained and corrected before approval. Local tests used Node v24.9.0; they do not establish success of the release's pinned Node image build.
- Rebuilt Linux images, signed publication and independent fresh-Kind customer acceptance remain required; none is claimed by this record.

## Verdict
Accept the narrowly scoped repair for hosted qualification and release rebuilding. Do not skip security updates or treat these local tests as complete beta acceptance.

Signed-off-by: Pal Lakatos-Toth <lakatos.toth.pal@gmail.com>
