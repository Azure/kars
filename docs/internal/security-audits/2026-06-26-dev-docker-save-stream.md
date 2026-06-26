# Security Audit — `kars dev` kind image load streams `docker save` (no maxBuffer overflow)

Date: 2026-06-26
Scope: `cli/src/commands/dev/local-k8s.ts` (`loadImageIntoKind`).
Gated path: `cli/src/commands/dev/local-k8s.ts`.

## Summary

`loadImageIntoKind` piped `execa(runtime, ["save", image])` into `ctr images
import`, but execa buffers child stdout into its result (capped at `maxBuffer`,
default 100 MB) **even when the stream is also piped**. A multi-hundred-MB image
(`openclaw-sandbox`) overflowed the cap and failed `kars dev` at "Loading kars
images". Fix: pass `{ buffer: false }` so the tarball streams straight to `ctr
import` and is never held in memory.

## T1: New capability / attack surface? (NO)
- Local developer-loop only (`kars dev --target local-k8s`, kind). No deployed
  resource, credential, network path, or policy is touched. Same two processes,
  same pipe; only execa's in-memory buffering is disabled.

## T2: Security-control change? (NO)
- No auth, crypto, image-provenance, or isolation change. The image bytes are
  the operator's own locally-built/loaded image, unchanged.

## T3: Availability / fail-open risk? (REDUCED)
- Removes a hard, size-dependent failure of `kars dev`. Pure reliability win;
  no fail-open (a genuine `docker save`/`ctr import` error still propagates).

## Verification
- CLI `tsc --noEmit` + oxlint clean; vitest green. Manual: `kars dev --target
  local-k8s` loads the large sandbox image without the maxBuffer error.

## Verdict
Accept. Local-dev reliability fix with no security-relevant surface.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
