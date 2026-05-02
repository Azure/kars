# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# Minimal BYO image meeting the AzureClaw BYO contract:
#   - declares org.azureclaw.runtime.contract = v1
#   - runs as UID 1000
#   - read-only root filesystem (writes in /sandbox + /tmp only)
#   - listens on :8080 for the in-cluster Service

FROM python:3.12-slim

LABEL org.azureclaw.runtime.contract="v1"
LABEL org.opencontainers.image.title="byo-quickstart"
LABEL org.opencontainers.image.description="AzureClaw BYO runtime quickstart example"

# Create the sandbox user that the controller will run as.
RUN groupadd -g 1000 sandbox && \
    useradd  -u 1000 -g 1000 -d /sandbox -m sandbox && \
    chown -R sandbox:sandbox /sandbox

WORKDIR /sandbox/app
COPY app/requirements.txt /sandbox/app/requirements.txt
RUN pip install --no-cache-dir -r requirements.txt

COPY app/main.py /sandbox/app/main.py
USER 1000

EXPOSE 8080
ENTRYPOINT ["uvicorn", "main:app", "--host", "0.0.0.0", "--port", "8080"]
