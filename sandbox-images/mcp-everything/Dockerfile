# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

FROM mcr.microsoft.com/azurelinux/base/nodejs:24@sha256:c30e39a396fb0ad2c7bf2fe6b4e2fe1598a9b79f09831c07d763fe09927e8779

WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci --omit=dev --ignore-scripts \
    && npm cache clean --force

ENV PORT=3001
EXPOSE 3001
USER 1000
ENTRYPOINT ["./node_modules/.bin/mcp-server-everything"]
CMD ["streamableHttp"]
