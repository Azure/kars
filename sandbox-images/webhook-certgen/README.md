# Admission certificate helper on Azure Linux 3

This keeps `jkroepke/kube-webhook-certgen` `1.8.3`, selected by the maintained
`kube-prometheus-stack` chart `85.3.3`. The checksum-pinned source is rebuilt
with Go `1.27.2`, `x/net` `0.60.0`, and frozen module locks. All upstream short
tests, including certificate generation and Kubernetes patching, run before
the static binary is produced.

```sh
make image-webhook-certgen
```

The final AL3 distroless image retains `/kube-webhook-certgen` and upstream
`create`/`patch` commands, runs as UID/GID `65534`, and contains no shell or
build tools. This target does not install, publish, grant access, or change
certificates. Complete-stack installer wiring remains a separate gate.

The upstream `version` subcommand reports `1.8.3`, with the pinned source commit's
timestamp for reproducible build metadata. Source checksum, executable hash,
image labels and retained Go build metadata establish its identity. Native
CLI execution and a complete image scan are distinct from full multi-stage or
AMD64 builds and actual installed admission certificate jobs.
