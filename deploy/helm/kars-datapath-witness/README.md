<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# kars-datapath-witness

Standalone optional chart. `enabled: false` by default: only a nonprivileged
operator-intent ConfigMap is rendered, no privileged workload or observation
cost. After enablement, off also retains the dedicated namespace to protect
unrelated resources. Helm owns its release history separately.

Use only release `kars-datapath-witness` in existing namespace `kars-system`.
An operator-provided, published aggregator image **digest** and explicit
`sandboxes` list are required when enabling. No image is assumed published.
This chart does not depend on Bridge or install/modify core Kars.

See [operator setup, image provenance, safe enable/disable and limitations](../../ebpf-witness/README.md)
in the public source, or
[the public source documentation](https://github.com/Azure/kars/blob/kars-bridge/deploy/ebpf-witness/README.md).
The daemon manifest is adapted from Inspektor Gadget v0.53.2 under Apache-2.0;
Kars integration changes are MIT. See `THIRD_PARTY_NOTICES.md` and `LICENSE-IG`.

Do not use Helm adoption, rollback, or direct uninstall while enabled.
The documented `enabled=false` upgrade checks ownership **before** removals.
An existing or shared IG installation requires separate operator review, even
when no witness ConfigMap exists. Bridge only copies commands and reads reports.
