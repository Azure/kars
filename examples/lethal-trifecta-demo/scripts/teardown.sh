#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
set -euo pipefail

echo "▸ tearing down lethal-trifecta demo"
kubectl delete namespace naked-claw                  --ignore-not-found
# Deleting the KarsSandbox CR (which lives in kars-claw) triggers
# the controller to garbage-collect the per-sandbox namespace
# (kars-realestate-agent); deleting both namespaces explicitly here
# is belt-and-braces in case the controller is offline.
kubectl delete namespace kars-claw              --ignore-not-found
kubectl delete namespace kars-realestate-agent  --ignore-not-found
echo "✅ done"
