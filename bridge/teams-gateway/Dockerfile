# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

FROM node:22-bookworm-slim AS builder
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci --ignore-scripts
COPY tsconfig.json ./
COPY src/ src/
RUN npx tsc

FROM node:22-bookworm-slim AS prod-deps
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci --omit=dev --ignore-scripts

FROM mcr.microsoft.com/azurelinux/distroless/base:3.0 AS runtime-base

# Export the complete Microsoft libstdc++ RPM payload and extend, rather than
# replace, the distroless package inventory. No Debian libraries are shipped.
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
ENV NODE_ENV=production
ENV HOME=/home/node NODE_EXTRA_CA_CERTS=/etc/pki/tls/certs/ca-bundle.crt
COPY --from=runtime-libs /runtime-libs/ /
COPY --from=builder /usr/local/bin/node /usr/local/bin/node
COPY --from=builder /usr/local/LICENSE /usr/local/share/licenses/node/LICENSE
COPY package.json ./
COPY --from=prod-deps /app/node_modules ./node_modules
COPY --from=builder /app/dist ./dist
RUN ["node", "-e", "const fs = require('node:fs'); if (process.versions.node.split('.')[0] !== '22') throw new Error('Node22 required'); require('node:tls').createSecureContext({ ca: fs.readFileSync(process.env.NODE_EXTRA_CA_CERTS) }); fs.appendFileSync('/etc/passwd', 'node:x:10001:10001::/home/node:/sbin/nologin\\n'); fs.appendFileSync('/etc/group', 'node:x:10001:\\n'); fs.mkdirSync('/home/node', { recursive: true }); fs.chownSync('/home/node', 10001, 10001);"]
USER 10001:10001
RUN ["node", "--input-type=module", "-e", "await import('./dist/main.js');"]
EXPOSE 3978 3979
HEALTHCHECK --interval=15s --timeout=5s --start-period=10s \
  CMD ["node", "-e", "fetch('http://localhost:3979/healthz').then(r=>{if(!r.ok)process.exit(1)}).catch(()=>process.exit(1))"]
ENTRYPOINT ["node"]
CMD ["dist/main.js"]
