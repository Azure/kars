# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
# Operator supplies an approved multi-platform Python 3.12+ Linux image by digest.
# No aggregator image has been published merely because this Dockerfile exists.
ARG PYTHON_BASE
FROM ${PYTHON_BASE} AS client
ARG PYTHON_BASE
ARG SOURCE_REVISION
ARG TARGETARCH
ENV TARGETARCH=${TARGETARCH}
COPY deploy/ebpf-witness/aggregator/fetch-client.py /tmp/fetch-client.py
RUN python3 /tmp/fetch-client.py

FROM ${PYTHON_BASE}
ARG SOURCE_REVISION
LABEL org.opencontainers.image.source="https://github.com/Azure/kars" \
      org.opencontainers.image.revision="${SOURCE_REVISION}" \
      org.opencontainers.image.licenses="MIT AND Apache-2.0" \
      io.kars.witness.ig-version="v0.53.2"
COPY --from=client /usr/local/bin/kubectl-gadget /usr/local/bin/kubectl-gadget
COPY --from=client /tmp/build.json /opt/witness/build.json
COPY deploy/ebpf-witness/aggregator/witness.py /opt/witness/witness.py
COPY deploy/helm/kars-datapath-witness/LICENSE-IG /usr/share/licenses/inspektor-gadget/LICENSE
COPY deploy/helm/kars-datapath-witness/THIRD_PARTY_NOTICES.md /usr/share/licenses/inspektor-gadget/NOTICE
ENV PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1 HOME=/tmp
USER 10001:10001
ENTRYPOINT ["python3", "/opt/witness/witness.py"]
