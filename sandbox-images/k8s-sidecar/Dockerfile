# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

FROM mcr.microsoft.com/azurelinux/base/python:3.12@sha256:71a1f5a1821f4c56e45aff52e792ed6c0500631e201cfc140c4c9d22886b29f9 AS build
ARG PIP_INDEX_URL=https://pypi.org/simple
ENV PYTHONDONTWRITEBYTECODE=1 PIP_DISABLE_PIP_VERSION_CHECK=1
WORKDIR /build
ADD --checksum=sha256:cc71d13773d6ea86ed5c9f1ccae7c2fd5c3a2febe5330e7bc663b44b9adc4bc9 https://codeload.github.com/kiwigrid/k8s-sidecar/tar.gz/070c102fac9bce0eeda44af52d87b6acdfd00f0c /build/source.tar.gz
COPY sandbox-images/k8s-sidecar/requirements.lock /build/
COPY sandbox-images/k8s-sidecar/build.py /usr/local/bin/build-sidecar.py
RUN python3 /usr/local/bin/build-sidecar.py /build

FROM mcr.microsoft.com/azurelinux/distroless/python:3.12@sha256:10bcfa3c563fd05f9b11495551bd36bfe919b4a2f32b68388e1c393bbeea64ac AS runtime
COPY --from=build /build/sidecar/ /opt/sidecar/
COPY --from=build /build/licenses/ /usr/share/licenses/k8s-sidecar/
LABEL org.opencontainers.image.version="2.7.3" \
      org.opencontainers.image.revision="070c102fac9bce0eeda44af52d87b6acdfd00f0c" \
      org.opencontainers.image.source="https://github.com/Azure/kars" \
      org.opencontainers.image.licenses="MIT"
ENV PYTHONPATH=/opt/sidecar/src:/opt/sidecar/site-packages \
    PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1 HOME=/tmp
USER 65534:65534
WORKDIR /opt/sidecar
ENTRYPOINT ["/usr/bin/python3", "-u", "-m", "sidecar"]
