# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

FROM mcr.microsoft.com/azurelinux/base/nodejs:24@sha256:c30e39a396fb0ad2c7bf2fe6b4e2fe1598a9b79f09831c07d763fe09927e8779

USER 0
RUN tdnf upgrade -y --refresh \
    && tdnf install -y alsa-lib at-spi2-atk at-spi2-core atk cairo cups-libs \
       dbus expat fontconfig dejavu-sans-fonts dejavu-sans-mono-fonts glib libX11 libXcomposite libXdamage \
       libXext libXfixes libXrandr libxcb libxkbcommon libdrm mesa-libgbm \
       nspr nss pango \
    && tdnf clean all

WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci --omit=dev --ignore-scripts && npm cache clean --force

ENV PLAYWRIGHT_BROWSERS_PATH=/ms-playwright
COPY browser-platform.sh image.test.mjs ./
RUN sh browser-platform.sh ./node_modules/.bin/playwright install chromium \
    && chmod -R a+rX /ms-playwright

ENV HOME=/tmp
EXPOSE 8931
USER 1000
ENTRYPOINT ["sh", "/app/browser-platform.sh", "/app/node_modules/.bin/playwright-mcp"]
CMD ["--port=8931", "--host=0.0.0.0", "--headless", "--browser=chromium", "--no-sandbox", "--isolated"]
