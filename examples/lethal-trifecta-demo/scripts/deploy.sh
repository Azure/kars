#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
set -euo pipefail

# Deploy both scenarios side-by-side.
# Prereq: an kars cluster (kars up) with kubectl context
# pointing at it, and a Foundry deployment configured.

cd "$(dirname "$0")/.."

echo "▸ creating namespaces"
kubectl apply -f scenarios/00-namespaces.yaml

echo "▸ deploying naked-claw (vanilla OpenClaw, domain-only allowlist)"
kubectl apply -f scenarios/01-naked-claw.yaml

echo "▸ deploying kars-claw (full KarsSandbox + InferencePolicy)"
kubectl apply -f scenarios/02-kars-sandbox.yaml

echo "▸ uploading poisoned skill to both namespaces"
# Replace the placeholder in 03-bait-server.yaml with the real bait body
SKILL_BODY=$(cat bait/poisoned-skill.md)
for ns in naked-claw kars-claw; do
  kubectl create configmap poisoned-skill \
    --namespace "$ns" \
    --from-file=poisoned-skill.md=bait/poisoned-skill.md \
    --dry-run=client -o yaml | kubectl apply -f -
done
kubectl apply -f scenarios/03-bait-server.yaml

echo "▸ waiting for pods..."
kubectl -n naked-claw                wait --for=condition=available --timeout=120s deploy/realestate-agent deploy/bait-server
# The KarsSandbox CR is created in `kars-claw`, but the kars
# controller materialises the sandbox deployment in its own per-sandbox
# namespace `kars-<sandbox-name>` (here: `kars-realestate-agent`).
# The bait server is a plain Deployment that stays in `kars-claw`.
kubectl -n kars-realestate-agent wait --for=condition=available --timeout=180s deploy/realestate-agent
kubectl -n kars-claw             wait --for=condition=available --timeout=180s deploy/bait-server

echo
echo "✅ deployed. Run ./scripts/run-attack.sh next."
