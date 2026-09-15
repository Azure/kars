#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# Run the BFF in the foreground. Existing listeners are never stopped or adopted.
set -euo pipefail
cd "$(dirname "$0")/bff"

exec cargo run --locked --bin kars-bridge-bff
