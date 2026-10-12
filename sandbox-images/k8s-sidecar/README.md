# Grafana configuration sidecar on Azure Linux 3

This packages upstream `kiwigrid/k8s-sidecar` `2.7.3` on AL3 distroless Python
`3.12`, within upstream's supported Python range. Source is checksum-pinned;
third-party wheels are version- and hash-locked. The upstream Python modules
are copied unchanged, including ConfigMap/Secret watching, file updates,
health endpoints, and reload requests.

```sh
make image-k8s-sidecar
```

`PIP_INDEX_URL` is a build-only argument for environments that require a package
mirror. Every wheel must still match the committed PyPI release hashes:

```sh
docker build --build-arg PIP_INDEX_URL=https://pypi.tuna.tsinghua.edu.cn/simple \
  -f sandbox-images/k8s-sidecar/Dockerfile -t kars-k8s-sidecar:latest .
```

The ARM64 package was resolved on AL3 Python 3.12, checked against official PyPI
release metadata before offline installation, and passed dependency checks.
The final-stage image passed non-root, read-only, offline imports including
native YAML/CFFI/cryptography operations. Its complete supported-package scan
covered 42 OS packages and all 22 locked Python dependencies with no HIGH or
CRITICAL findings, including unfixed issues and without ignores. This is not
an AMD64 or full multi-stage build qualification.

The runtime uses UID/GID `65534`, contains no shell or installer, and needs no
runtime package download. Its environment-variable interface remains the
upstream interface. Optional `SCRIPT` hooks requiring a shell are not supported
by this distroless image; the maintained Grafana stack does not configure one.

On native ARM64 Kind, a temporary namespace also passed actual ConfigMap initial
sync, watched additions/updates/deletions, HTTP health, and rejection of Secret
listing with namespace-scoped ConfigMap-only RBAC. The namespace was removed and
existing workloads were unchanged. This does not establish Grafana reload or
full monitoring-stack acceptance.
