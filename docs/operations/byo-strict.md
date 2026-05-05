# BYO Strict-Mode Admission

**Status:** GA. Default `false` for backward compatibility; recommended `true` in production.

## What it does

When `controller.byoStrict=true`, the controller validates the
`spec.runtime.byo` block of every `ClawSandbox` of `runtime.kind=BYO`
before reconciling. Any violation fails the sandbox closed:

```yaml
status:
  conditions:
    - type: Degraded
      status: "True"
      reason: BYOContractInvalid
      message: "byo.contractVersion: byo.contractVersion=`v999` is not in the supported set [\"v1\", \"1\", \"1.0\"]"
```

No Deployment, Service, or NetworkPolicy is rendered while the
contract is invalid — the sandbox is in a clean failed state that the
operator can re-roll cleanly after fixing the CR.

When `byoStrict=false` (default), the same checks run as advisories:
the controller logs `WARN`, optionally records a `RuntimeReady` /
`BYOContractAdvisory` condition, and proceeds with the Deployment.
Pre-strict-mode behaviour is bit-for-bit preserved when the flag is
`false`.

## What it checks (CR-level)

| Field                | Check                                                                     |
|----------------------|---------------------------------------------------------------------------|
| `byo.contractVersion`| Required; must be one of `v1`, `1`, `1.0` (set `SUPPORTED_BYO_CONTRACT_VERSIONS`). |
| `byo.image`          | Required; must look like `host/path:tag` or `host/path@sha256:<digest>`.  |

These are **CR-level** checks. They don't require registry I/O and
catch the most common operator mistakes (typoed contract version,
missing tag, leading whitespace).

## What it does *not* check (yet)

Registry-side label introspection — i.e. confirming the image
actually carries `org.azureclaw.runtime.contract=v1` — is a v1.1
follow-on. That requires an authenticated registry pull in the
controller's hot reconcile loop, which is a substantial new
dependency surface (rate limits, image-cache, registry auth flow).
v1.0 ships CR-level enforcement only, with a clear extension point:
see `controller/src/reconciler/byo_contract.rs` and the doc comment
under `## Two layers of validation`.

## Enabling

Helm:

```yaml
controller:
  byoStrict: true
```

Or directly via env var on the controller Deployment:

```yaml
env:
  - name: BYO_STRICT_MODE
    value: "1"
```

The controller logs a single line at startup:

```
INFO BYO strict-mode enabled — invalid BYO contracts will be rejected
```

## Migration

If you have existing BYO sandboxes:

1. Roll out the controller with `byoStrict=false` (default). No
   behaviour change.
2. Inspect the controller logs for `WARN` lines tagged `BYO contract
   advisory (warn-only)`. Each one is a sandbox that would fail under
   strict mode.
3. Fix the CRs (declare `byo.contractVersion: v1`, fix typoed image
   tags).
4. Re-roll the controller with `byoStrict=true`.

## Related

- [`docs/runtimes.md`](../runtimes.md) — the BYO runtime contract
- [`examples/byo-quickstart/`](../../examples/byo-quickstart/) — minimal working sandbox
- [`controller/src/reconciler/byo_contract.rs`](../../controller/src/reconciler/byo_contract.rs) — implementation
