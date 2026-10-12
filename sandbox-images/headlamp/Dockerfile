# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

ARG AZURELINUX_DISTROLESS=mcr.microsoft.com/azurelinux/distroless/base:3.0
FROM ghcr.io/headlamp-k8s/headlamp@sha256:89c6c65810bfde61796483c93c70d659104355593792bf55cab680d685da8eeb AS upstream

FROM golang:1.27.2-bookworm AS backend
ENV CGO_ENABLED=0 GOMAXPROCS=2 GOMEMLIMIT=3GiB GOTOOLCHAIN=local
ADD --checksum=sha256:73c885c09f4121f8497ced73b99e8bda233f354ae328c7a5211106577e4a5b5c https://codeload.github.com/kubernetes-sigs/headlamp/tar.gz/066c5be9deb14b12bcef3485b531c64a84a2a219 /download/source.tar.gz
COPY sandbox-images/headlamp/build-backend.sh /usr/local/bin/build-headlamp-backend
RUN sh /usr/local/bin/build-headlamp-backend /download

FROM node:22-bookworm-slim AS frontend
ARG NPM_CONFIG_REGISTRY=https://registry.npmjs.org
WORKDIR /build
ADD --checksum=sha256:73c885c09f4121f8497ced73b99e8bda233f354ae328c7a5211106577e4a5b5c https://codeload.github.com/kubernetes-sigs/headlamp/tar.gz/066c5be9deb14b12bcef3485b531c64a84a2a219 /build/source.tar.gz
RUN tar --no-same-owner -xzf source.tar.gz --strip-components=1 \
    headlamp-066c5be9deb14b12bcef3485b531c64a84a2a219/frontend \
    headlamp-066c5be9deb14b12bcef3485b531c64a84a2a219/app/package.json
COPY sandbox-images/headlamp/build-frontend.mjs /build/frontend/kars-build.mjs
WORKDIR /build/frontend
RUN node kars-build.mjs prepare \
    && npm install --package-lock-only --ignore-scripts --no-audit --no-fund --registry="${NPM_CONFIG_REGISTRY}" \
    && npm ci --omit=dev --no-audit --no-fund --registry="${NPM_CONFIG_REGISTRY}"
RUN node --max-old-space-size=3072 kars-build.mjs build

FROM node:22-bookworm-slim AS plugin
ARG NPM_CONFIG_REGISTRY=https://registry.npmjs.org
WORKDIR /build
COPY tools/headlamp-plugin/package.json tools/headlamp-plugin/package-lock.json ./
RUN npm ci --no-audit --no-fund --registry="${NPM_CONFIG_REGISTRY}"
COPY tools/headlamp-plugin/tsconfig.json ./
COPY tools/headlamp-plugin/src ./src
RUN npm run build

FROM ${AZURELINUX_DISTROLESS}
COPY --from=frontend /build/frontend/build /headlamp/frontend
COPY --from=frontend /build/frontend/frontend-evidence.json /build/frontend/frontend.cdx.json /usr/share/kars/headlamp/
COPY --from=upstream /headlamp/plugins /headlamp/plugins
COPY --from=upstream /headlamp/static-plugins /headlamp/static-plugins
COPY --from=backend /download/headlamp-server /headlamp/headlamp-server
COPY --from=plugin /build/dist /headlamp/plugins/kars
COPY --from=plugin /build/package.json /headlamp/plugins/kars/package.json
COPY LICENSE /usr/share/licenses/kars/LICENSE
ADD --chmod=0444 --checksum=sha256:cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30 https://raw.githubusercontent.com/kubernetes-sigs/headlamp/v0.41.0/LICENSE /usr/share/licenses/headlamp/LICENSE
LABEL org.opencontainers.image.title="Kars Headlamp" \
      org.opencontainers.image.description="Headlamp v0.41.0 and the Kars plugin on Azure Linux 3 distroless" \
      org.kars.headlamp.backend-patch="go1.27.2-dependencies.1" \
      org.kars.headlamp.frontend-patch="npm-runtime-dependencies.1" \
      org.opencontainers.image.source="https://github.com/Azure/kars" \
      org.opencontainers.image.licenses="Apache-2.0 AND MIT"
ENV HEADLAMP_STATIC_PLUGINS_DIR=/headlamp/static-plugins \
    USER=headlamp \
    HOME=/tmp
USER 10001:10001
EXPOSE 4466
ENTRYPOINT ["/headlamp/headlamp-server", "-html-static-dir", "/headlamp/frontend", "-plugins-dir", "/headlamp/plugins"]
