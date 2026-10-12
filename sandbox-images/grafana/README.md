<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Optional Kars Grafana image

`make image-grafana` builds `$(REGISTRY)/kars-grafana:latest` from Grafana
13.0.10 source with Go 1.27.2. Both the builder and distroless runtime use
Azure Linux 3. Grafana stays optional; this target does not change the default
monitoring installation or enable it in an existing Core release.

The backend uses fixed x/net, Tempo, Prometheus and Loki dependencies.
Promlib 0.0.11 supplies the matching PromQL parser migration; two Grafana
call sites use the instance-based parser with the same default options.
Grafana's original plugin SDK 0.291.0 is retained because the unrelated
0.292.0 API change removes the query-schema `Examples` field.
Six affected upstream test suites and the query-schema package compilation
run before the full backend build. Module checksums remain enabled, and the
image includes backend build information, workspace replacements and module
locks under `/usr/share/kars/grafana/`.

The frontend is the checksummed official distribution for the selected
ARM64 or AMD64 platform. The original backend is replaced, and the entire
unused Elasticsearch plugin is excluded. Kars dashboards and provisioning
do not use Elasticsearch; the available signed plugin still has vulnerable
dependencies. This is removal of an unused component, not modification of
signed bytes or an unsigned-plugin exception. Remaining distribution files
and signature enforcement are preserved. Runtime plugin preinstallation
and update checks are disabled.

The Tempo replacement's historical Go pseudoversion starts with `v1.5.1`,
but its commit `4aeafc237b8d9a8d62e45735131e8a89eb741a00` is exactly the
upstream `v2.10.3` release. Version-only scanners can therefore report
CVE-2026-21728 and CVE-2026-28377 against this fixed source. Preserve the raw
scan and verify the source-to-release mapping; do not hide findings or
mislabel the Go module as a different version.

Packaging helper tests:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover -s sandbox-images/grafana -p 'test_*.py'
```

The maintained backend script has run successfully on native ARM64 within
4 GiB memory, two CPUs and 8 GiB working storage. It reproduced the qualified
candidate's backend binary, build information and workspace byte for byte;
all 96 archived module-lock files also had identical contents.
Offline HTTP/database/Prometheus datasource checks passed for that candidate.
This evidence is not a full Dockerfile rebuild, AMD64 runtime qualification,
installed dashboards or Kind customer acceptance.
