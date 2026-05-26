#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# setup.sh — resolve host.docker.internal and render the kars-mesh preset
#
# Resolves host.docker.internal via DNS and substitutes the __HOST_IP__
# placeholder in the preset template. This handles platform differences:
#   macOS Docker Desktop:  192.168.65.254
#   Linux Docker:          172.17.0.1
#   WSL2:                  varies
#
# Usage:
#   ./setup.sh                       # renders to stdout
#   ./setup.sh --install             # copies rendered preset into NemoClaw blueprint
#   ./setup.sh --install --apply     # also applies to a running sandbox

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEMPLATE="${SCRIPT_DIR}/policies/presets/kars-mesh.yaml"
NEMOCLAW_PRESETS="${NEMOCLAW_PRESETS:-${HOME}/.nemoclaw/source/nemoclaw-blueprint/policies/presets}"

# --- Resolve host.docker.internal -----------------------------------------

resolve_host_ip() {
  local ip

  # Explicit override
  if [[ -n "${HOST_IP:-}" ]]; then echo "$HOST_IP"; return; fi

  # Try getent first (Linux)
  if command -v getent &>/dev/null; then
    ip=$(getent hosts host.docker.internal 2>/dev/null | awk '{print $1}')
    if [[ -n "$ip" ]]; then echo "$ip"; return; fi
  fi

  # nslookup — only trust answers with a valid "Name:" section (not NXDOMAIN)
  if command -v nslookup &>/dev/null; then
    local out
    out=$(nslookup host.docker.internal 2>/dev/null)
    if echo "$out" | grep -q "^Name:"; then
      ip=$(echo "$out" | awk '/^Name:/{found=1; next} found && /^Address:/{print $2; exit}')
      if [[ -n "$ip" ]]; then echo "$ip"; return; fi
    fi
  fi

  # dig
  if command -v dig &>/dev/null; then
    ip=$(dig +short host.docker.internal 2>/dev/null | head -1)
    if [[ -n "$ip" ]]; then echo "$ip"; return; fi
  fi

  # /etc/hosts (set inside Docker containers)
  ip=$(grep -m1 host.docker.internal /etc/hosts 2>/dev/null | awk '{print $1}')
  if [[ -n "$ip" ]]; then echo "$ip"; return; fi

  # Platform defaults — useful when running on the host (outside Docker)
  case "$(uname -s)" in
    Darwin) echo "192.168.65.254"; return ;;   # Docker Desktop on macOS
    Linux)  echo "172.17.0.1";     return ;;   # default docker0 bridge
  esac

  echo >&2 "ERROR: cannot resolve host.docker.internal and no platform default"
  echo >&2 "       Set HOST_IP=x.x.x.x explicitly."
  exit 1
}

HOST_IP=$(resolve_host_ip)
echo "Resolved host.docker.internal → ${HOST_IP}" >&2

# --- Render the preset template -------------------------------------------

if [[ ! -f "$TEMPLATE" ]]; then
  echo >&2 "ERROR: template not found: ${TEMPLATE}"
  exit 1
fi

RENDERED=$(sed "s/__HOST_IP__/${HOST_IP}/g" "$TEMPLATE")

# --- Output or install ----------------------------------------------------

if [[ "${1:-}" == "--install" ]]; then
  mkdir -p "$NEMOCLAW_PRESETS"
  echo "$RENDERED" > "${NEMOCLAW_PRESETS}/kars-mesh.yaml"
  echo "Installed preset → ${NEMOCLAW_PRESETS}/kars-mesh.yaml" >&2

  if [[ "${2:-}" == "--apply" ]]; then
    # Find the first running sandbox and apply
    SANDBOX=$(docker ps --filter "name=openshell" --format '{{.Names}}' | head -1)
    if [[ -n "$SANDBOX" ]]; then
      echo "Applying preset to sandbox: ${SANDBOX}" >&2
      nemoclaw "${SANDBOX}" policy-add kars-mesh 2>&1 || true
    else
      echo "No running sandbox found. Apply manually:" >&2
      echo "  nemoclaw <sandbox-name> policy-add kars-mesh" >&2
    fi
  fi
else
  echo "$RENDERED"
fi
