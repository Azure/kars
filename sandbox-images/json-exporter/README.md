# JSON exporter on Azure Linux 3

The AgentMesh monitoring exporter remains upstream `json_exporter` `0.7.0`.
Checksum-pinned source and frozen Go module locks are built with Go `1.27.2`;
all upstream short tests run before producing the static executable.
The dependency lock includes `x/oauth2` `0.27.0`, the first fix for
`CVE-2025-22868`, alongside `x/net` `0.60.0` and `x/crypto` `0.57.0`.

```sh
make image-json-exporter
```

The AL3 distroless image preserves `/bin/json_exporter`, port `7979`, and the
upstream `/probe` and `--config.file` interfaces used by
`deploy/helm/kars/templates/monitoring-json-exporter.yaml`. It runs as UID/GID `65534`
without a shell or build tools. A configuration file must be supplied, as in
the upstream image. The target does not install or publish anything.

Native CLI and complete package scans are not installed relay-health metric
acceptance, full multi-stage build acceptance, or cross-architecture proof.
