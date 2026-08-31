#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
set -euo pipefail

source_dir="${KARS_WORKSPACE_BOOTSTRAP_SOURCE:-/etc/kars/workspace-bootstrap}"
destination_dir="${KARS_WORKSPACE_BOOTSTRAP_DESTINATION:-/sandbox/.openclaw/workspace}"
state_dir="${KARS_WORKSPACE_BOOTSTRAP_STATE_DIR:-/sandbox/.kars}"
policy="${KARS_WORKSPACE_OVERWRITE_POLICY:-IfMissing}"
config_map_uid="${KARS_WORKSPACE_BOOTSTRAP_CONFIG_MAP_UID:-}"
resource_version="${KARS_WORKSPACE_BOOTSTRAP_RESOURCE_VERSION:-}"

case "$policy" in
  IfMissing|Always) ;;
  *)
    echo "workspace bootstrap received invalid overwrite policy: $policy" >&2
    exit 1
    ;;
esac

reject_symlink_components() {
  local path="$1"
  local current=""
  local component
  if [[ "$path" != /* ]]; then
    echo "workspace bootstrap requires an absolute destination path: $path" >&2
    exit 1
  fi
  IFS='/' read -r -a components <<< "$path"
  for component in "${components[@]}"; do
    [ -n "$component" ] || continue
    current="$current/$component"
    if [ -L "$current" ]; then
      echo "workspace bootstrap refused symlink path component: $current" >&2
      exit 1
    fi
    if [ -e "$current" ] && [ ! -d "$current" ]; then
      echo "workspace bootstrap path component is not a directory: $current" >&2
      exit 1
    fi
  done
}

reject_symlink_components "$destination_dir"
reject_symlink_components "$state_dir"
mkdir -p "$destination_dir" "$state_dir"
umask 027
manifest_tmp=$(mktemp "$state_dir/.bootstrap-state.XXXXXX")
copy_tmp=""
cleanup() {
  [ -z "$copy_tmp" ] || rm -f -- "$copy_tmp"
  rm -f -- "$manifest_tmp"
}
trap cleanup EXIT HUP INT TERM

printf '{"configMapUid":"%s","resourceVersion":"%s","policy":"%s","files":{' \
  "$config_map_uid" "$resource_version" "$policy" > "$manifest_tmp"
first_file=true
for filename in AGENTS.md SOUL.md HEARTBEAT.md TOOLS.md USER.md; do
  source_file="$source_dir/$filename"
  destination_file="$destination_dir/$filename"
  [ -f "$source_file" ] || continue

  if [ -L "$destination_file" ]; then
    echo "workspace bootstrap refused symlink destination: $filename" >&2
    exit 1
  fi
  if [ "$policy" = "Always" ] || [ ! -e "$destination_file" ]; then
    copy_tmp=$(mktemp "$destination_dir/.${filename}.kars-bootstrap.XXXXXX")
    cat -- "$source_file" > "$copy_tmp"
    chmod 0640 "$copy_tmp"
    mv -f -- "$copy_tmp" "$destination_file"
    copy_tmp=""
  fi

  digest=$(sha256sum "$destination_file" | awk '{print $1}')
  if [ "$first_file" = true ]; then
    first_file=false
  else
    printf ',' >> "$manifest_tmp"
  fi
  printf '"%s":"%s"' "$filename" "$digest" >> "$manifest_tmp"
done
printf '}}\n' >> "$manifest_tmp"
chmod 0640 "$manifest_tmp"
mv -f -- "$manifest_tmp" "$state_dir/bootstrap-state.json"
trap - EXIT HUP INT TERM
