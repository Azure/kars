# kube-state-metrics on Azure Linux 3

This rebuilds the existing monitoring chart's `kube-state-metrics` `2.19.0`
from checksum-pinned upstream source. Go `1.27.2` and frozen dependency locks
provide security updates without changing the collector release. All upstream
unit packages run before producing the static binary; the upstream Kubernetes
E2E suite is excluded from this image build and remains an installed gate.

```sh
make image-kube-state-metrics
```

The AL3 distroless runtime preserves `/kube-state-metrics` and the upstream
8080/8081 metric and telemetry ports. It runs as UID/GID `65534` without a
shell or build tools. Building does not install or publish the image.
Native CLI and package-scan qualification do not establish installed resource
collection, full multi-stage builds, or acceptance on another architecture.
