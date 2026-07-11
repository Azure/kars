FROM node:22-alpine@sha256:16e22a550f3863206a3f701448c45f7912c6896a62de43add43bb9c86130c3e2

WORKDIR /app

COPY package.json package-lock.json ./
RUN npm ci --omit=dev --ignore-scripts \
    && npm cache clean --force

ENV PORT=3001
EXPOSE 3001

USER 1000
ENTRYPOINT ["./node_modules/.bin/mcp-server-everything"]
CMD ["streamableHttp"]
