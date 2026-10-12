# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# Toolchain only. The matching chart supplies its generated schema-hook.mjs.
# Build from the repository root on a native amd64 or arm64 builder.
FROM node:22-bookworm-slim AS node
FROM golang:1.27.2-bookworm AS tools
ENV CGO_ENABLED=0 GOMAXPROCS=2 GOMEMLIMIT=3GiB GOTOOLCHAIN=local
ADD --checksum=sha256:e113bad2e11b9920a2d1ad2c1ee8b1881796330ba02e2673f5312ba10990800e https://codeload.github.com/helm/helm/tar.gz/bec5b06ed841fe5269972d864d5177944fd5970f /download/helm.tar.gz
ADD --checksum=sha256:3020026c65ea6701c42113ca092bb20bff71099028f0cf596354e95d53f7e0a6 https://codeload.github.com/kubernetes/kubernetes/tar.gz/329000efd0b26bd78155e51e4df449130f0fca00 /download/kubernetes.tar.gz
COPY cli/schema-hook/build-tools.sh /usr/local/bin/build-schema-tools
RUN sh /usr/local/bin/build-schema-tools /download

FROM mcr.microsoft.com/azurelinux/distroless/base:3.0 AS runtime-base
FROM mcr.microsoft.com/azurelinux/base/core:3.0 AS runtime-libs
COPY --from=runtime-base /var/lib/rpmmanifest/ /runtime-libs/var/lib/rpmmanifest/
RUN set -eu; \
    rpm -V libstdc++; \
    grep -Fx "$(rpm -q libgcc)" /runtime-libs/var/lib/rpmmanifest/container-manifest-1; \
    glibc_prefix="$(rpm -q --qf '%{NAME}-%{VERSION}-' glibc)"; \
    glibc_arch="$(rpm -q --qf '%{ARCH}' glibc)"; \
    matches=0; \
    while IFS= read -r package; do \
      case "$package" in "$glibc_prefix"?*".$glibc_arch") matches=$((matches + 1));; esac; \
    done < /runtime-libs/var/lib/rpmmanifest/container-manifest-1; \
    [ "$matches" -eq 1 ]; \
    rpm -ql libstdc++ > /package-files; \
    while IFS= read -r path; do \
      if [ -d "$path" ]; then \
        mkdir -p "/runtime-libs$(readlink -f "$path")"; \
      else \
        directory="$(readlink -f "$(dirname "$path")")"; \
        mkdir -p "/runtime-libs$directory"; \
        cp -a "$path" "/runtime-libs$directory/"; \
      fi; \
    done < /package-files; \
    rpm -q libstdc++ >> /runtime-libs/var/lib/rpmmanifest/container-manifest-1; \
    rpm -q --qf '%{NAME}\t%{VERSION}-%{RELEASE}\t%{INSTALLTIME}\t%{BUILDTIME}\t%{VENDOR}\t%{EPOCH}\t%{SIZE}\t%{ARCH}\t%{EPOCHNUM}\t%{SOURCERPM}\n' libstdc++ \
      >> /runtime-libs/var/lib/rpmmanifest/container-manifest-2

FROM runtime-base AS runtime
LABEL org.kars.schema-toolchain.patch="go1.27.2-xnet0.60.0"
ENV PATH=/usr/local/bin HOME=/tmp LD_BIND_NOW=1 NODE_OPTIONS=--max-old-space-size=160
ENV NODE_EXTRA_CA_CERTS=/etc/pki/tls/certs/ca-bundle.crt SSL_CERT_FILE=/etc/pki/tls/certs/ca-bundle.crt
WORKDIR /opt/kars-schema
COPY --from=runtime-libs /runtime-libs/ /
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/LICENSE /usr/local/share/licenses/node/LICENSE
COPY --from=tools /download/helm /usr/local/bin/helm
COPY --from=tools /download/helm.LICENSE /usr/local/share/licenses/helm/LICENSE
COPY --from=tools /download/kubectl /usr/local/bin/kubectl
COPY --from=tools /download/kubectl.LICENSE /usr/local/share/licenses/kubectl/LICENSE
RUN ["node", "-e", "const fs=require('node:fs'); if(process.versions.node.split('.')[0]!=='22')throw new Error('Node22 required'); require('node:tls').createSecureContext({ca:fs.readFileSync(process.env.NODE_EXTRA_CA_CERTS)}); fs.appendFileSync('/etc/passwd','schema-hook:x:10001:10001::/tmp:/sbin/nologin\\n'); fs.appendFileSync('/etc/group','schema-hook:x:10001:\\n');"]
USER 10001:10001
RUN ["helm", "version", "--short"]
RUN ["kubectl", "version", "--client", "-o", "json"]
ENTRYPOINT ["node"]
CMD ["/opt/kars-schema/schema-hook.mjs"]
