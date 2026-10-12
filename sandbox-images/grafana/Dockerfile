# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

ARG AZURELINUX_DISTROLESS=mcr.microsoft.com/azurelinux/distroless/base:3.0
FROM mcr.microsoft.com/azurelinux/base/core:3.0 AS build
ARG TARGETARCH
RUN tdnf install -y ca-certificates curl golang python3 tar gzip findutils \
    && tdnf clean all
ENV CGO_ENABLED=0 GOMAXPROCS=2 GOMEMLIMIT=3GiB GOGC=50 \
    GOTOOLCHAIN=go1.27.2 GOPATH=/build/go GOCACHE=/build/cache GOTMPDIR=/build/tmp
WORKDIR /build
ADD --checksum=sha256:7172f17ec16650d86c41dd2aa6b54afbb6cbab3e0906b10d973f72f0c942e0e0 https://codeload.github.com/grafana/grafana/tar.gz/952c9f4a26f5fa9be8dd49e979328966e8f1ee24 /inputs/source.tar.gz
COPY sandbox-images/grafana/build-backend.sh sandbox-images/grafana/no-cache.py sandbox-images/grafana/patch-parser.py /inputs/
RUN mkdir -p /build/tmp \
    && sh /inputs/build-backend.sh /build/output /inputs \
    && rm -rf /build/go /build/cache /build/tmp /build/output/source
RUN case "$TARGETARCH" in \
      arm64) checksum=ab6b328f029690f851567c4ac2f4706903da90e2dba5059351ecada3f0df55bc ;; \
      amd64) checksum=d76e886048f8a378558db8e9556a8c12fc417aef4145754e1bf77acb253d33ae ;; \
      *) echo "Unsupported Grafana architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac \
    && curl --fail --location --retry 2 --max-time 300 --max-filesize 536870912 \
      -o /build/distribution.tar.gz \
      "https://dl.grafana.com/grafana/release/13.0.10/grafana_13.0.10_36453862914_linux_${TARGETARCH}.tar.gz" \
    && echo "$checksum  /build/distribution.tar.gz" | sha256sum --check --strict \
    && mkdir -p /out/grafana/bin \
    && tar --no-same-owner --strip-components=1 \
      --exclude=grafana-13.0.10/bin/grafana \
      --exclude=grafana-13.0.10/data/plugins-bundled/elasticsearch \
      -xzf /build/distribution.tar.gz -C /out/grafana \
    && cp /build/output/grafana /out/grafana/bin/grafana \
    && test ! -e /out/grafana/data/plugins-bundled/elasticsearch \
    && rm /build/distribution.tar.gz

FROM ${AZURELINUX_DISTROLESS}
COPY --from=build /out/grafana/ /usr/share/grafana/
COPY --from=build /build/output/grafana.buildinfo /build/output/go.work /build/output/module-locks.tar.gz /usr/share/kars/grafana/
WORKDIR /usr/share/grafana
ENV GF_PATHS_HOME=/usr/share/grafana GF_PATHS_DATA=/var/lib/grafana \
    GF_PATHS_LOGS=/var/log/grafana GF_PATHS_PLUGINS=/var/lib/grafana/plugins \
    GF_PLUGINS_PREINSTALL_DISABLED=true GF_ANALYTICS_CHECK_FOR_UPDATES=false \
    GF_ANALYTICS_CHECK_FOR_PLUGIN_UPDATES=false
LABEL org.opencontainers.image.title="Kars Grafana" \
      org.opencontainers.image.version="13.0.10" \
      org.opencontainers.image.revision="952c9f4a26f5fa9be8dd49e979328966e8f1ee24" \
      org.opencontainers.image.source="https://github.com/Azure/kars" \
      org.opencontainers.image.licenses="AGPL-3.0-only" \
      org.kars.grafana.distribution="kars-monitoring-without-unused-elasticsearch"
USER 472:472
EXPOSE 3000
ENTRYPOINT ["/usr/share/grafana/bin/grafana"]
CMD ["server"]
