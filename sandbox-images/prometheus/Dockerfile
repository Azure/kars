# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

ARG AZURELINUX_DISTROLESS=mcr.microsoft.com/azurelinux/distroless/base:3.0
FROM node:22-bookworm-slim AS frontend
ARG NPM_CONFIG_REGISTRY=https://registry.npmjs.org
ENV NODE_OPTIONS=--max-old-space-size=3072 TMPDIR=/build/tmp
WORKDIR /build
ADD --checksum=sha256:4c2bb19d8503537858f8167f5369a424cf9db095f38d5f0aebcea974f452344f https://codeload.github.com/prometheus/prometheus/tar.gz/eb173f5256d4022afba1e9bc3d19740a76859fae /build/source.tar.gz
RUN mkdir source tmp && tar --no-same-owner -xzf source.tar.gz --strip-components=1 -C source
COPY sandbox-images/prometheus/build-frontend.mjs /build/build-frontend.mjs
WORKDIR /build/source/web/ui
RUN node /build/build-frontend.mjs prepare \
    && npm install --prefix react-app --package-lock-only --ignore-scripts --no-audit --no-fund \
    && npm ci --prefix react-app --no-audit --no-fund \
    && npm run build --prefix react-app -- --stats \
    && node /build/build-frontend.mjs inventory legacy \
    && mkdir -p static \
    && mv react-app/build static/react-app \
    && find react-app/node_modules -type f \( -iname 'LICENSE*' -o -iname 'NOTICE*' \) -print0 > /build/legacy-license-files \
    && tar --null -T /build/legacy-license-files -czf /build/legacy-licenses.tar.gz \
    && rm -rf /build/source/web/ui/react-app/node_modules /root/.npm /build/tmp \
    && mkdir /build/tmp
RUN npm install --package-lock-only --ignore-scripts --no-audit --no-fund \
    && npm update nanoid postcss react-router react-router-dom --package-lock-only --ignore-scripts --no-audit --no-fund \
    && npm ci --no-audit --no-fund \
    && npm run build:module \
    && ./node_modules/.bin/tsc -b mantine-ui \
    && node /build/build-frontend.mjs inventory mantine \
    && mv mantine-ui/dist static/mantine-ui \
    && tar -czf /build/ui.tar.gz static \
    && find node_modules mantine-ui module -type f \( -iname 'LICENSE*' -o -iname 'NOTICE*' \) -print0 > /build/mantine-license-files \
    && tar --null -T /build/mantine-license-files -czf /build/mantine-licenses.tar.gz

FROM golang:1.27.2-bookworm AS backend
ENV CGO_ENABLED=0 GOMAXPROCS=2 GOMEMLIMIT=3GiB GOTOOLCHAIN=local
WORKDIR /download
ADD --checksum=sha256:4c2bb19d8503537858f8167f5369a424cf9db095f38d5f0aebcea974f452344f https://codeload.github.com/prometheus/prometheus/tar.gz/eb173f5256d4022afba1e9bc3d19740a76859fae /download/source.tar.gz
ADD --checksum=sha256:b67b29c18ea7bca108e86c176c77fa2a528cfccca63d5c9831eb3d223dd0ac43 https://github.com/prometheus/prometheus/commit/39672984b5cc9f7fb61dfcf6df5693b1137ca6cb.patch /download/moby.patch
ADD --checksum=sha256:450b3f274a62a62de50d3af569647fe0bb6a24a024c39afab17d6a4849ca7354 https://github.com/prometheus/prometheus/commit/ab63eeb0d228a3cf70c15b8181da05831db93a76.patch /download/moby-api.patch
COPY --from=frontend /build/ui.tar.gz /download/ui.tar.gz
COPY sandbox-images/prometheus/prometheus.go.mod sandbox-images/prometheus/prometheus.go.sum /download/
COPY sandbox-images/prometheus/build-backend.sh /usr/local/bin/build-prometheus-backend
RUN sh /usr/local/bin/build-prometheus-backend /download \
    && install -d -m 0755 /download/config /download/licenses \
    && install -m 0444 /download/prometheus.yml /download/config/prometheus.yml \
    && install -m 0444 /download/LICENSE /download/NOTICE /download/licenses/

FROM ${AZURELINUX_DISTROLESS}
COPY --from=backend --chmod=0555 /download/prometheus /download/promtool /bin/
COPY --from=backend /download/config/ /etc/prometheus/
COPY --from=backend --chown=65534:65534 /download/data/ /prometheus/
COPY --from=backend /download/licenses/ /usr/share/licenses/prometheus/
COPY --from=frontend /build/legacy-licenses.tar.gz /build/mantine-licenses.tar.gz /usr/share/licenses/prometheus/
COPY --from=frontend /build/source/web/ui/legacy.cdx.json /build/source/web/ui/legacy-evidence.json /build/source/web/ui/mantine.cdx.json /build/source/web/ui/mantine-evidence.json /usr/share/kars/prometheus/
LABEL org.opencontainers.image.title="Kars Prometheus" \
      org.opencontainers.image.version="3.11.3" \
      org.opencontainers.image.revision="eb173f5256d4022afba1e9bc3d19740a76859fae" \
      org.kars.prometheus.security-backport="go1.27.2-xnet0.60-moby-client.1" \
      org.kars.prometheus.frontend-patch="nanoid-postcss-react-router.1" \
      org.opencontainers.image.source="https://github.com/Azure/kars" \
      org.opencontainers.image.licenses="Apache-2.0"
ENV HOME=/tmp
USER 65534:65534
EXPOSE 9090
ENTRYPOINT ["/bin/prometheus"]
CMD ["--config.file=/etc/prometheus/prometheus.yml", "--storage.tsdb.path=/prometheus"]
