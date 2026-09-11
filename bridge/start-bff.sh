#!/usr/bin/env bash
# Run the BFF in the foreground. Existing listeners are never stopped or adopted.
set -euo pipefail
cd "$(dirname "$0")/bff"

exec cargo run --locked --bin kars-bridge-bff
