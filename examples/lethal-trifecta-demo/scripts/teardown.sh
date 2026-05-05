#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
set -euo pipefail

echo "▸ tearing down lethal-trifecta demo"
kubectl delete namespace naked-claw      --ignore-not-found
kubectl delete namespace azureclaw-claw  --ignore-not-found
echo "✅ done"
