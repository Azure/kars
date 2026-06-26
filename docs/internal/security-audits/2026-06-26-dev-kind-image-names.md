# Security Audit — `kars dev` loads local `:dev` image names into kind (not ACR `:latest`)

Date: 2026-06-26
Scope: `cli/src/commands/dev/local-k8s.ts` (`runLocalK8s` image target table).
Gated path: `cli/src/commands/dev/local-k8s.ts`.

## Summary

On `kars dev --target local-k8s`, the workloads run with `imagePullPolicy:
Never`, which does a literal image-name match. `values-local-dev.yaml` pins the
controller, sandbox, and inference-router to local `:dev` tags, but the kind
image-load table targeted `karsacr.azurecr.io/...:latest` names instead. The
node ended up with the ACR names while pods requested `:dev` →
`ErrImageNeverPull`, the controller never started, and `kars dev` timed out at
"Waiting for sandbox pod". Fix: target the exact `:dev` names the workloads
reference, keeping the ACR `:latest` names as retag **aliases** (fallback
sources).

## T1: New capability / attack surface? (NO)
- Local developer-loop only (kind). Renames which local image tags are loaded /
  retagged into the kind node. No deployed resource, credential, registry
  auth, or network path changes. `imagePullPolicy: Never` means nothing is
  pulled from any registry.

## T2: Security-control change? (NO)
- No auth, crypto, provenance, or isolation change. Images are the operator's
  locally-built artifacts; the AKS/`kars up` ACR image strings are untouched
  (they remain the controller's fallback when `*_IMAGE` env is unset).

## T3: Availability / fail-open risk? (REDUCED)
- Fixes a hard `kars dev` bring-up failure (ErrImageNeverPull). No fail-open:
  a missing local image still surfaces as a normal load error.

## Verification
- CLI `tsc --noEmit` + oxlint clean; vitest green. Manual: `kars dev --target
  local-k8s` brings the controller + sandbox Ready without ErrImageNeverPull.

## Verdict
Accept. Local-dev correctness fix; no security-relevant surface.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
