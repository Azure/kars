#!/bin/sh
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
set -eu

# Playwright has no AL3 download label. Use its Ubuntu 24 browser binary with
# native AL3 libraries; image tests qualify that combination on each CPU.
case "$(uname -m)" in
    aarch64) export PLAYWRIGHT_HOST_PLATFORM_OVERRIDE=ubuntu24.04-arm64 ;;
    x86_64) export PLAYWRIGHT_HOST_PLATFORM_OVERRIDE=ubuntu24.04 ;;
    *) echo "Unsupported managed browser architecture" >&2; exit 1 ;;
esac
exec "$@"
