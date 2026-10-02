# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# Toolchain only. The matching chart supplies its generated schema-hook.mjs.
# Build from the repository root. ARM builds require the reviewed ARM tool hashes.
FROM node:22-bookworm-slim AS tools
ARG TARGETARCH
ARG HELM_SHA256=86584a54def73570558f66f5111cc53dfed56689637ae32c1201205d494f54fb
ARG KUBECTL_SHA256=3cfeaf80be482b435b0aa214aff6e0b2c312ee23c0ff20810c75517b6004c6eb
RUN test "$TARGETARCH" = amd64 || test "$TARGETARCH" = arm64
ADD --checksum=sha256:${HELM_SHA256} https://get.helm.sh/helm-v4.3.0-linux-${TARGETARCH}.tar.gz /download/helm.tar.gz
ADD --checksum=sha256:${KUBECTL_SHA256} https://dl.k8s.io/release/v1.35.9/bin/linux/${TARGETARCH}/kubectl /download/kubectl
ADD https://raw.githubusercontent.com/kubernetes/kubernetes/v1.35.9/LICENSE /download/kubectl.LICENSE
RUN tar --no-same-owner -xzf /download/helm.tar.gz -C /download && \
    cp /download/linux-${TARGETARCH}/helm /download/helm && \
    cp /download/linux-${TARGETARCH}/LICENSE /download/helm.LICENSE && \
    chmod 0555 /download/helm /download/kubectl

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
ENV PATH=/usr/local/bin HOME=/tmp LD_BIND_NOW=1 NODE_OPTIONS=--max-old-space-size=160
ENV NODE_EXTRA_CA_CERTS=/etc/pki/tls/certs/ca-bundle.crt SSL_CERT_FILE=/etc/pki/tls/certs/ca-bundle.crt
WORKDIR /opt/kars-schema
COPY --from=runtime-libs /runtime-libs/ /
COPY --from=tools /usr/local/bin/node /usr/local/bin/node
COPY --from=tools /usr/local/LICENSE /usr/local/share/licenses/node/LICENSE
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
