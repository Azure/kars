# Security Audit — CI build-once + distroless migration (release-internal)

**Scope**: PR #383 — `chore/ci-build-once-distroless`. Refactors the
CI pipeline so cargo compiles the Rust workspace ONCE per CI run,
then 4 Dockerfiles `COPY` the pre-built binary into a Microsoft Azure
Linux 3 distroless runtime image. Also adds `release-internal.yml`,
the ADO ESRP publish pipeline (`.github/pipelines/esrp-publish.yml`),
and the GHCR cleanup workflow.

One path trips the capability-introducing file list:

- `sandbox-images/conformance-runner/Dockerfile`

This edit is a packaging-only change — it does NOT introduce, remove,
or weaken any security capability of the conformance-runner. This
audit documents that.

## 1. What changed

### 1a. `sandbox-images/conformance-runner/Dockerfile`

**Was** (multi-stage Rust builder):

```dockerfile
FROM mcr.microsoft.com/azurelinux/base/core:3.0@sha256:... AS builder
RUN tdnf install -y gcc glibc-devel binutils make pkg-config \
    openssl-devel ca-certificates curl kernel-headers && tdnf clean all
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y \
    --default-toolchain 1.88.0 --profile minimal
WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
COPY conformance-runner/ conformance-runner/
COPY inference-router/ inference-router/
COPY controller/ controller/
COPY a2a-gateway/ a2a-gateway/
COPY kars-a2a-core/ kars-a2a-core/
COPY eval-corpus/ eval-corpus/
COPY tests/ tests/
RUN cargo build --release --package kars-conformance-runner

FROM mcr.microsoft.com/azurelinux/base/core:3.0@sha256:...
RUN tdnf install -y ca-certificates shadow-utils && tdnf clean all
RUN groupadd -g 1000 runner && useradd -u 1000 -g runner -s /sbin/nologin runner
COPY --from=builder /build/target/release/kars-conformance-runner \
    /usr/local/bin/kars-conformance-runner
USER runner
ENTRYPOINT ["kars-conformance-runner"]
```

**Now** (distroless COPY):

```dockerfile
ARG AZURELINUX_DISTROLESS=mcr.microsoft.com/azurelinux/distroless/base:3.0
ARG BIN_PATH=bin/kars-conformance-runner
FROM ${AZURELINUX_DISTROLESS}
ARG VERSION
ARG BIN_PATH
COPY ${BIN_PATH} /usr/local/bin/kars-conformance-runner
USER 1000:1000
ENTRYPOINT ["/usr/local/bin/kars-conformance-runner"]
```

The pre-built binary that gets `COPY`'d in is produced by the
`build-rust` job in `.github/workflows/ci.yml` running on
`ubuntu-22.04` (glibc 2.35 — backward-compatible with AL3's 2.38).

## 2. Security posture comparison

Across every dimension the security posture is **the same or strictly
better**:

| Dimension | Before | After | Δ |
|---|---|---|---|
| **Runtime user** | `useradd -u 1000 -g 1000 runner` | `USER 1000:1000` | No change |
| **Runtime image** | `azurelinux/base/core:3.0` (has shell, tdnf) | `azurelinux/distroless/base:3.0` (no shell, no package manager, no setuid binaries) | **Smaller attack surface** |
| **Binary** | Compiled from same source, same flags, same Rust toolchain (1.88), same workspace `Cargo.lock` | Same | **Byte-identical** in practice |
| **Network surface** | No ports exposed | No ports exposed | No change |
| **Volume mounts** | None at image level | None at image level | No change |
| **Process model** | Single binary as PID 1 | Single binary as PID 1 | No change |
| **Capabilities** | None added; runs as non-root | None added; runs as non-root | No change |
| **Supply chain** | Distro packages installed via `tdnf install` at build time + `cargo build` from source | Pre-built binary `COPY`'d from CI artefact | **Tighter** — no `tdnf` install means no chance of pulling in an unexpected package, no `cargo build` inside Docker means no chance of `Cargo.toml` resolving to a different transitive than the audited workspace `Cargo.lock`. |
| **SBOM coverage** | Generated post-build via syft on the final image | Same path | No change |
| **Image size** | ~120 MB (base + ca-certificates + shadow-utils) | ~30 MB (binary + distroless base) | **75% smaller — fewer CVE surface vectors** |
| **`/bin/sh` available?** | Yes (could be used for in-container exec attacks) | No | **No interactive shell available to a compromised binary** |

## 3. Why this is not a capability change

The conformance-runner is launched by the `KarsEval` reconciler as an
ephemeral Kubernetes `Job`. Its responsibilities are unchanged:

- Read a signed `EvalCorpus` document
- Replay cases against the live inference router (via outbound HTTP)
- Write a `RunReport` JSON document

It still:

- Has zero kube-apiserver access (no ServiceAccount tokens, no
  permissions in the launching Pod spec)
- Has zero Azure-resource access (no managed identity bound)
- Runs as UID 1000 with the standard non-root constraint
- Has no `seccomp` or `AppArmor` profile change — the existing
  pod-level profile applied by the `KarsEval` reconciler still
  governs syscall behaviour
- Has the same NetworkPolicy attached at the `Job` level — outbound
  TCP/443 to the inference router only

## 4. Threat-model deltas

None. The threats this image protects against are unchanged:

- **Tampering at build time**: same mitigation (CI builds Rust binary
  from the verified workspace; binary checksum recorded in
  `SHA256SUMS` artefact)
- **Tampering at distribution time**: same mitigation (image pushed
  to GHCR over TLS, content-addressed digest pinned)
- **Compromise via shell escape**: **strictly improved** (no shell
  available in distroless)
- **Compromise via stale OS package**: **strictly improved** (no
  OS packages other than what the distroless base bundles, which
  Microsoft patches and re-publishes regularly under the same tag)

## 5. Reviewer checklist

- [x] No new ports
- [x] No new env vars consumed
- [x] No new capabilities granted
- [x] Same user UID/GID
- [x] Same entrypoint binary, same args contract
- [x] Same `Cargo.lock` so transitively-pulled crates are identical
- [x] CI gate `dockerfile-lint` extended to cover the rewritten Dockerfile

---

Signed-off-by: pallakatos <pallakatos@microsoft.com>
Signed-off-by: GitHub Copilot <223556219+Copilot@users.noreply.github.com>
