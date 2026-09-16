# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# kars Bridge web (Next.js) — container image. Multi-stage standalone build, so
# the runtime image ships only server.js + the minimal traced node_modules.
# Cloud-agnostic: runs identically on AKS, EKS, GKE, and local kind.
#
# Build from the web/ directory as context:
#   docker build -f web/Dockerfile -t <registry>/kars-bridge-web:<tag> web
FROM node:22-bookworm-slim AS deps
WORKDIR /app
COPY package.json package-lock.json* ./
RUN npm ci

FROM node:22-bookworm-slim AS build
WORKDIR /app
COPY --from=deps /app/node_modules ./node_modules
COPY . .
ENV NEXT_TELEMETRY_DISABLED=1
RUN npm run build

FROM mcr.microsoft.com/azurelinux/distroless/base:3.0 AS runtime-base

# The upstream Node22 binary and sharp need libstdc++. Export the complete
# Microsoft RPM payload, not Debian libraries, and preserve the base inventory.
FROM mcr.microsoft.com/azurelinux/base/core:3.0 AS runtime-libs
COPY --from=runtime-base /var/lib/rpmmanifest/ /runtime-libs/var/lib/rpmmanifest/
RUN set -eu; \
    rpm -V libstdc++; \
    for package in glibc libgcc; do \
      grep -Fx "$(rpm -q "$package")" /runtime-libs/var/lib/rpmmanifest/container-manifest-1; \
    done; \
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
WORKDIR /app
ENV NODE_ENV=production NEXT_TELEMETRY_DISABLED=1 PORT=3000
ENV HOME=/home/next NODE_EXTRA_CA_CERTS=/etc/pki/tls/certs/ca-bundle.crt
COPY --from=runtime-libs /runtime-libs/ /
COPY --from=build /usr/local/bin/node /usr/local/bin/node
COPY --from=build /usr/local/LICENSE /usr/local/share/licenses/node/LICENSE
# Standalone output: server + traced deps + static assets.
COPY --from=build /app/.next/standalone ./
COPY --from=build /app/.next/static ./.next/static
COPY --from=build /app/public ./public
RUN ["node", "-e", "const fs = require('node:fs'); if (process.versions.node.split('.')[0] !== '22') throw new Error('Node22 required'); require('node:tls').createSecureContext({ ca: fs.readFileSync(process.env.NODE_EXTRA_CA_CERTS) }); fs.appendFileSync('/etc/passwd', 'next:x:10001:10001::/home/next:/sbin/nologin\\n'); fs.appendFileSync('/etc/group', 'next:x:10001:\\n'); for (const path of ['/home/next', '/app/.next/cache']) { fs.mkdirSync(path, { recursive: true }); fs.chownSync(path, 10001, 10001); }"]
USER 10001:10001
# Exercise the native addon against the final Microsoft libraries, not Debian.
RUN ["node", "-e", "const sharp = require('node:module').createRequire(require.resolve('next/package.json'))('sharp'); sharp({ create: { width: 2, height: 2, channels: 4, background: '#000000' } }).png().toBuffer().then(data => sharp(data).metadata()).then(info => { if (info.format !== 'png' || info.width !== 2 || info.height !== 2) throw new Error('sharp runtime verification failed'); }).catch(error => { console.error(error); process.exit(1); });"]
EXPOSE 3000
# BRIDGE_BFF_URL points at the in-cluster BFF Service (set by the Helm chart).
ENTRYPOINT ["node", "server.js"]
