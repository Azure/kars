# Image Supply Chain Security — Notation + Ratify

## Overview
kars uses Notation for image signing and Ratify for admission verification.
Only signed images from the trusted ACR can be deployed as sandbox containers.

## Setup

### 1. Install Notation CLI
```bash
brew install notation  # macOS
```

### 2. Sign images during build
```bash
# Generate a key (one-time)
notation key generate kars-signing

# Sign the image after push
notation sign karsacr.azurecr.io/kars-controller:0.1.0
notation sign karsacr.azurecr.io/kars-inference-router:0.1.0
notation sign karsacr.azurecr.io/openclaw-sandbox:latest
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
        - name: kars-images
          registryScopes:
            - "karsacr.azurecr.io/*"
          signatureVerification:
            level: strict
          trustStores:
            - "ca:kars-signing"
```

### 5. Gatekeeper constraint
```yaml
apiVersion: constraints.gatekeeper.sh/v1beta1
kind: K8sAllowedImages
metadata:
  name: kars-signed-images-only
spec:
  match:
    namespaces:
      - "kars-*"
  parameters:
    allowedImages:
      - "karsacr.azurecr.io/*"
```

## Automation

> **Status: manual / roadmap.** The Notation signing + Ratify admission steps
> in this document are applied **manually** today (the `az`/`kubectl` commands
> above). There is **no** `kars up --sign-images` flag in the CLI — automating
> this into the `kars up` flow is tracked on the [roadmap](../../docs/roadmap.md).
> Note that the **public release pipeline already cosign-signs every image**
> (keyless OIDC) with an SPDX SBOM and SLSA provenance; the Notation/Ratify path
> here is an *additional*, optional in-cluster enforcement layer for operators who
> standardise on Notation.
