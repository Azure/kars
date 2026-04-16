#!/usr/bin/env bash
# setup.sh — resolve host.docker.internal and render the azureclaw-mesh preset
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
TEMPLATE="${SCRIPT_DIR}/policies/presets/azureclaw-mesh.yaml"
NEMOCLAW_PRESETS="${HOME}/.nemoclaw/source/nemoclaw-blueprint/policies/presets"

# --- Resolve host.docker.internal -----------------------------------------

resolve_host_ip() {
  local ip

  # Try getent first (Linux)
  if command -v getent &>/dev/null; then
    ip=$(getent hosts host.docker.internal 2>/dev/null | awk '{print $1}')
    if [[ -n "$ip" ]]; then echo "$ip"; return; fi
  fi

  # Fall back to nslookup
  if command -v nslookup &>/dev/null; then
    ip=$(nslookup host.docker.internal 2>/dev/null | awk '/^Address:/{a=$2} END{print a}')
    if [[ -n "$ip" && "$ip" != "#53" ]]; then echo "$ip"; return; fi
  fi

  # Fall back to dig
  if command -v dig &>/dev/null; then
    ip=$(dig +short host.docker.internal 2>/dev/null | head -1)
    if [[ -n "$ip" ]]; then echo "$ip"; return; fi
  fi

  # Fall back to /etc/hosts
  ip=$(grep -m1 host.docker.internal /etc/hosts 2>/dev/null | awk '{print $1}')
  if [[ -n "$ip" ]]; then echo "$ip"; return; fi

  echo >&2 "ERROR: cannot resolve host.docker.internal"
  echo >&2 "       Are you running inside a Docker/NemoClaw container?"
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
  echo "$RENDERED" > "${NEMOCLAW_PRESETS}/azureclaw-mesh.yaml"
  echo "Installed preset → ${NEMOCLAW_PRESETS}/azureclaw-mesh.yaml" >&2

  if [[ "${2:-}" == "--apply" ]]; then
    # Find the first running sandbox and apply
    SANDBOX=$(docker ps --filter "name=openshell" --format '{{.Names}}' | head -1)
    if [[ -n "$SANDBOX" ]]; then
      echo "Applying preset to sandbox: ${SANDBOX}" >&2
      nemoclaw "${SANDBOX}" policy-add azureclaw-mesh 2>&1 || true
    else
      echo "No running sandbox found. Apply manually:" >&2
      echo "  nemoclaw <sandbox-name> policy-add azureclaw-mesh" >&2
    fi
  fi
else
  echo "$RENDERED"
fi
