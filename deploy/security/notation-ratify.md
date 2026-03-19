# Image Supply Chain Security — Notation + Ratify

## Overview
AzureClaw uses Notation for image signing and Ratify for admission verification.
Only signed images from the trusted ACR can be deployed as sandbox containers.

## Setup

### 1. Install Notation CLI
```bash
brew install notation  # macOS
```

### 2. Sign images during build
```bash
# Generate a key (one-time)
notation key generate azureclaw-signing

# Sign the image after push
notation sign azureclawacr.azurecr.io/azureclaw-controller:0.1.0
notation sign azureclawacr.azurecr.io/azureclaw-inference-router:0.1.0
notation sign azureclawacr.azurecr.io/openclaw-sandbox:latest
```

### 3. Install Ratify on AKS
```bash
helm repo add ratify https://ratify-project.github.io/ratify
helm install ratify ratify/ratify \
  --namespace gatekeeper-system \
  --set featureFlags.RATIFY_CERT_ROTATION=true \
  --set azureWorkloadIdentity.clientId=${WI_CLIENT_ID}
```

### 4. Configure Ratify to verify Notation signatures
```yaml
apiVersion: config.ratify.deislabs.io/v1beta1
kind: Store
metadata:
  name: oras
spec:
  name: oras
  parameters:
    cosignEnabled: false
    notationEnabled: true
---
apiVersion: config.ratify.deislabs.io/v1beta1
kind: Verifier
metadata:
  name: notation
spec:
  name: notation
  artifactTypes: application/vnd.cncf.notary.signature
  parameters:
    trustPolicyDoc:
      version: "1.0"
      trustPolicies:
        - name: azureclaw-images
          registryScopes:
            - "azureclawacr.azurecr.io/*"
          signatureVerification:
            level: strict
          trustStores:
            - "ca:azureclaw-signing"
```

### 5. Gatekeeper constraint
```yaml
apiVersion: constraints.gatekeeper.sh/v1beta1
kind: K8sAllowedImages
metadata:
  name: azureclaw-signed-images-only
spec:
  match:
    namespaces:
      - "azureclaw-*"
  parameters:
    allowedImages:
      - "azureclawacr.azurecr.io/*"
```

## Automation
The `azureclaw up` flow will include Notation signing + Ratify deployment
when `--sign-images` flag is provided.
