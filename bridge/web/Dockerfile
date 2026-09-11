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
# The in-cluster BFF is reached at the Service DNS name; the browser talks to the
# same-origin /api/* rewrite, so this is baked at build for the rewrite target.
ENV NEXT_TELEMETRY_DISABLED=1
RUN npm run build

FROM node:22-bookworm-slim AS runtime
WORKDIR /app
ENV NODE_ENV=production NEXT_TELEMETRY_DISABLED=1 PORT=3000
RUN useradd --uid 10001 --user-group --home-dir /home/next --create-home next
# Standalone output: server + traced deps + static assets.
COPY --from=build /app/.next/standalone ./
COPY --from=build /app/.next/static ./.next/static
COPY --from=build /app/public ./public
USER 10001
EXPOSE 3000
# BRIDGE_BFF_URL points at the in-cluster BFF Service (set by the Helm chart).
ENTRYPOINT ["node", "server.js"]
