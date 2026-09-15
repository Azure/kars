<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License for this notice's Kars-specific text. -->

# Inspektor Gadget

Copyright The Inspektor Gadget authors.
Licensed under Apache License 2.0; the full license is in `LICENSE-IG`.

The daemon's container, mounts, capabilities, readiness command and config
conventions in `templates/gadget.yaml` are adapted from
[Inspektor Gadget v0.53.2's daemonset template](https://github.com/inspektor-gadget/inspektor-gadget/blob/v0.53.2/charts/gadget/templates/daemonset.yaml).
The OCI verification public key in `templates/configmaps.yaml` is the official
[v0.53.2 chart key](https://github.com/inspektor-gadget/inspektor-gadget/blob/v0.53.2/charts/gadget/values.yaml).
The upstream chart archive used for comparison has SHA-256
`94344e27350dba5843dc3826a3b2d9c1c05aec3ec1f4af4388983d80bf5e392e`.
The archive is not a runtime dependency and is not republished in this chart.

Kars modifications: isolated ownership/namespaces, explicit opt-in, pinned
images, bounded resources, host BTF init check, modern AppArmor field, no
host CRI-O/NRI hook installation or global cleanup, a fixed fanotify+ebpf mode,
and reduced read-only IG API authority. These integration changes are MIT;
they do not relicense upstream material.

The separately built aggregator image includes the unmodified official
`kubectl-gadget` v0.53.2 executable under Apache-2.0. Its source and release
archives are at https://github.com/inspektor-gadget/inspektor-gadget/tree/v0.53.2.
The upstream release provides per-architecture client BOMs. Retain those and
the operator image's generated SBOM when redistributing the image, including
licenses for transitive components and the Python base. The digest-pinned IG
container and OCI BPF gadgets remain upstream distributions under their own
licenses; they are referenced, not copied into the chart or aggregator.
