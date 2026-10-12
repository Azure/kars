# Prometheus Operator on Azure Linux 3

The operator and config reloader retain the upstream `0.90.1` version used by
`kube-prometheus-stack` `85.3.3`. Both are compiled from the same checksum-pinned
source archive with Go `1.27.2` and the checked-in dependency locks. The final
images use Azure Linux 3 distroless, UID/GID `65534`, and upstream entry points.
No shell or build toolchain is included in either final image.

```sh
make image-prometheus-operator
```

This explicit target builds both `:latest` images; it does not push, install, or
replace any cluster resources. The default monitoring installer is not yet
switched to these images: the complete stack must be qualified first.

The dependency repair retains the operator API version and behavior while
updating the Go standard library, `x/net`, `x/crypto`, `x/tools`, and gRPC.
The embedded Prometheus module is updated to `0.311.3` to repair its reported
HIGH findings, with `prom-label-proxy` `0.13.0` for the corresponding parser API.
The build applies exact-match namespace-label and rule-validation parser
compatibility changes while preserving the default parser options.
An explicit replacement retains Alertmanager `0.31.1`, whose configuration API
is consumed by operator `0.90.1`; remove it when upgrading that consumer.
Upstream namespace-enforcement, rule and Alertmanager regression tests run too.
The build runs focused upstream operator, Prometheus, and reloader tests.
Go build information is retained so image scanning can inventory both binaries.

The Microsoft-packaged `0.90.1` candidate was rejected: its native AL3 version
check passed, but its image scan reported two HIGH OpenSSL package findings and
did not inventory the Go executable. Repackaging that executable alone would
not establish complete dependency coverage.

Native source-build and image evidence are separate from a complete multi-stage
recipe build, AMD64 execution, and installed operator/reload reconciliation.
None of those later gates is implied by a successful compilation.
