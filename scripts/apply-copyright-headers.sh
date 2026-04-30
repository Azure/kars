#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
# scripts/apply-copyright-headers.sh — one-shot idempotent header applier.
# Run from repo root. Idempotent: running twice is a no-op.
set -euo pipefail

SLASH_HEADER="// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License."

HASH_HEADER="# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License."

applied=0
skipped=0

while IFS= read -r file; do
  # Skip if already has the header
  if head -5 "$file" | grep -qE '^(//|#) *Copyright \(c\) Microsoft Corporation'; then
    ((skipped++)) || true
    continue
  fi

  # Determine comment style by extension
  ext="${file##*.}"
  case "$ext" in
    rs|ts|tsx|js) header="$SLASH_HEADER" ;;
    sh)           header="$HASH_HEADER" ;;
    *)            continue ;;
  esac

  # Read file content
  content=$(<"$file")

  # Handle shebang
  first_line=$(head -1 "$file")
  if [[ "$first_line" == '#!'* ]]; then
    rest=$(tail -n +2 "$file")
    printf '%s\n%s\n\n%s\n' "$first_line" "$header" "$rest" > "$file"
  else
    printf '%s\n\n%s\n' "$header" "$content" > "$file"
  fi

  ((applied++)) || true
done < <(
  git ls-files \
    | grep -E '\.(rs|ts|tsx|js|sh)$' \
    | grep -v '^vendor/' \
    | grep -v 'node_modules/' \
    | grep -v '/dist/' \
    | grep -v '^target/' \
    | grep -v '/build/' \
    | grep -v '\.d\.ts$' \
    | grep -v '\.turbo/' \
    | grep -v '/coverage/'
)

echo "✅ Applied headers to $applied file(s). Skipped $skipped (already had header)."
