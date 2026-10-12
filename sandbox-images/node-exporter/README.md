# Node exporter on Azure Linux 3

This retains node exporter `1.11.1`, as selected by the maintained
`kube-prometheus-stack` chart `85.3.3`. The checksum-pinned source is rebuilt
with Go `1.27.2`, `x/net` `0.60.0`, `x/crypto` `0.57.0`, and checked-in module
locks. The build unpacks the upstream collector fixtures and runs all upstream
short tests before producing the static executable with retained Go inventory.

```sh
make image-node-exporter
```

The final image contains the executable and licenses on AL3 distroless, runs as
UID/GID `65534`, and retains the upstream entry point and port. This target does
not push or install anything. Monitoring installer wiring awaits qualification
of the complete stack.

The existing Microsoft `1.11.1-19` image ran correctly on native ARM64 AL3, but
its complete image scan could not inventory the Go executable. The rebuilt
image must pass both native execution and complete supported-package scanning;
compilation alone is not qualification. Full multi-stage/AMD64 execution and
installed node metrics are separate acceptance gates.
