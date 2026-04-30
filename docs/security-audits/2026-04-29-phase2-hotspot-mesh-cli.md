# Phase 2 / S15.b — `cli/src/commands/mesh.ts` hotspot decomposition

**Date:** 2026-04-29
**Slice:** `phase2-hotspot-mesh-cli`
**Sub-slice of:** S15 `phase2-hotspot-pass3` (§4.2 file-budget enforcement)

## Summary

Per `docs/implementation-plan.md` §4.2, `cli/src/commands/mesh.ts` carried a Phase 2 cap of **800 LOC**. Pre-slice it stood at **1583 LOC** — a 783-line debt accumulated through Phase 1 mesh work. This slice decomposes the file along its natural seams, bringing it to **667 LOC** while preserving:

- The full subcommand surface (`mesh auth`, `status`, `list`, `reset`, `security`, `peer`, `unpair`, `promote`, `demote`).
- All public re-exports already imported by `mesh.test.ts` (`generateKeypair`, `base58Encode`, `encryptPrivateKey`, `decryptPrivateKey`, `checkRegistryHealth`, `checkRelayHealth`, `killProcessesOnPorts`, `killStaleListeners`, type `MeshIdentity`).
- All identity/file path conventions, including 0o600/0o700 mode bits on `~/.azureclaw/mesh-identity.json`.

## Existing implementation surveyed

(§0.2 #8 anti-duplication discipline.)

- `cli/src/commands/mesh.ts` (1583 LOC) — the only owner of the mesh CLI surface; nothing else implements identity persistence, OAuth callback, or registry promote.
- `cli/src/commands/mesh.test.ts` — imports the listed public symbols from `./mesh.js`; **must continue to compile and pass without changes** (verified: 28/28 tests still green).
- `cli/src/stepper.ts` — `banner`, `section`, `kvLine`, `checkLine` consumed unchanged.
- `cli/src/config.ts` — `loadContext`, `saveContext` consumed unchanged.
- No second copy of identity loading, AES-GCM at-rest encryption, base58 encoding, OAuth callback HTTP server, or LB/port-forward promote logic exists anywhere in the tree (verified by grep on `generateKeypair`, `base58Encode`, `IDENTITY_FILE`, `waitForOAuthCallback`, `checkRegistryHealth` outside mesh.ts).

No new module was introduced where extending an existing one would have sufficed; the new files live in a fresh `cli/src/commands/mesh/` directory that did not exist before, preserving the 1:1 mapping of file → concern that the rest of the CLI codebase already follows (`commands/handoff/`, `commands/operator/`).

## Decomposition

| New file | Concern | LOC | Public exports |
|---|---|---|---|
| `cli/src/commands/mesh/identity.ts` | Identity persistence + at-rest crypto + Ed25519 keypair + AMID | 137 | `MeshIdentity`, `IDENTITY_DIR`, `IDENTITY_FILE`, `generateKeypair`, `base58Encode`, `encryptPrivateKey`, `decryptPrivateKey`, `loadIdentity`, `saveIdentity` |
| `cli/src/commands/mesh/oauth.ts` | OAuth callback HTTP server + HTML/log escapers | 94 | `OAuthResult`, `escapeHtml`, `sanitizeForLog`, `waitForOAuthCallback` |
| `cli/src/commands/mesh/health.ts` | Port + WebSocket health helpers | 127 | `killProcessesOnPorts`, `killStaleListeners`, `findDuplicateListeners`, `checkRegistryHealth`, `checkRelayHealth` |
| `cli/src/commands/mesh/auth.ts` | `mesh auth` subcommand body | 221 | `attachAuthSubcommand(cmd)` |
| `cli/src/commands/mesh/promote.ts` | `mesh promote` subcommand body | 409 | `attachPromoteSubcommand(cmd)` |

`mesh.ts` retains: subcommand definitions for `status`, `list`, `reset`, `security`, `peer`, `unpair`, `demote` (smaller, tightly-coupled handlers that share inline state with the Commander wiring) plus calls to the two `attach*Subcommand` helpers.

## Verification

| Gate | Result |
|---|---|
| `cli/src/commands/mesh.ts` LOC | 1583 → **667** (under §4.2 cap of 800) |
| `npx tsc --noEmit` | clean |
| `npm test -- --run` | 454/454 pass (2 skipped, pre-existing) |
| `npm run lint` | 0 errors (27 warnings, all pre-existing in `plugin.ts`) |
| `npm run build` | success; `dist/` regenerated |
| `mesh.test.ts` (most-affected) | 28/28 pass without modification |

## Behavior delta

**None.** Every helper was moved verbatim; every subcommand action retains its full body byte-for-byte (only the function-extraction wrapper was added). The closure-captured variables of the `promote` and `auth` actions never reached into outer-`meshCommand()` lexical state — they consumed only top-level helpers (now imported) and shared `commander` + `console` + `process` globals — so the action-scope semantics are preserved.

CLI users see no change in flag set, output, exit codes, file paths, or environment-variable consumption.

## Threat-model considerations

No new attack surface. The slice does not change:

- Identity file location (`~/.azureclaw/mesh-identity.json`) or permission bits (0o700 dir, 0o600 file).
- AES-256-GCM key derivation (machine-bound seed `azureclaw:mesh-identity:<hostname>:<homedir>`).
- OAuth callback bind address (`127.0.0.1` only) or timeout (5 minutes).
- HTML/log escaper logic (`escapeHtml` / `sanitizeForLog` lifted unchanged).
- Registry/relay health probe logic, port-forward PID file handling, or LoadBalancer service patches.

Custom-crypto lint (`ci/no-custom-crypto.sh`) continues to pass — the moved code is the same code (Node `crypto.createCipheriv` + `crypto.generateKeyPairSync`) we were already shipping.

## Sign-offs

- Core: ✅ — module split is a pure refactor; behavior preserved; LOC budget enforced; no new public surface.
- Security: ✅ — no change to identity-at-rest encryption, OAuth bind/timeout, log/HTML escapers, or file mode bits; threat model unchanged.


Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
