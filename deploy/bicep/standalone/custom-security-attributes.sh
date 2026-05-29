#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# kars custom security attributes bootstrap.
#
# Declares the `AgentGovernance` custom security attribute set on the
# current Entra tenant and four attribute definitions inside it:
#
#   AgentGovernance
#     ├── AgentClassification : String   ["Standard", "Restricted", "Confidential"]
#     ├── DataSensitivity     : String   ["Public", "Internal", "Confidential"]
#     ├── ProductOwner        : String   (Entra user OID)
#     └── ManagedBy           : String   (free-form; kars uses "kars-controller")
#
# Once applied, kars KarsSandbox CRs can reference these attributes via
# `spec.meshAuth.customSecurityAttributes` and the controller will
# PATCH them onto each per-sandbox agent identity SP at provisioning
# time. Conditional Access policies can then target these attributes
# (see `conditional-access-baseline.bicep`).
#
# Required role: `Attribute Definition Administrator` (or higher) on
# the tenant. This is a ONE-TIME bootstrap; running it again is a
# safe no-op because each step is conditional on the resource not
# already existing.
#
# Why not Bicep? The `Microsoft.Graph` Bicep extension (v1.0) does not
# yet ship typed resource declarations for `attributeSets` and
# `customSecurityAttributeDefinitions`. The Graph REST API is the
# canonical mechanism; this script uses `az rest` which goes through
# the same auth path as the rest of kars' Graph integration.
#
# Usage:
#   ./custom-security-attributes.sh
#   ATTRIBUTE_SET_NAME=AgentGovernance ./custom-security-attributes.sh
#   SEARCHABLE=true ./custom-security-attributes.sh   # default: false
#
# Output: stdout summary of each create/skip step + final
#   "Tenant attribute set ready" line.

set -euo pipefail

ATTRIBUTE_SET_NAME="${ATTRIBUTE_SET_NAME:-AgentGovernance}"
SEARCHABLE="${SEARCHABLE:-false}"

GRAPH_BASE="https://graph.microsoft.com/beta"

if ! command -v az >/dev/null 2>&1; then
  echo "ERROR: az CLI not found on PATH. Install from https://learn.microsoft.com/cli/azure/install-azure-cli" >&2
  exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "ERROR: jq not found on PATH. Required for JSON parsing." >&2
  exit 1
fi

# Verify az is signed in.
if ! az account show -o none 2>/dev/null; then
  echo "ERROR: Azure CLI is not signed in. Run 'az login' first." >&2
  exit 1
fi
TENANT_ID="$(az account show --query tenantId -o tsv)"
echo "Tenant: ${TENANT_ID}"
echo "Attribute set: ${ATTRIBUTE_SET_NAME}"
echo "Searchable: ${SEARCHABLE}"
echo

# Step 1: ensure the attribute set exists.
EXISTING_SET="$(az rest --method GET \
  --url "${GRAPH_BASE}/directory/attributeSets/${ATTRIBUTE_SET_NAME}" \
  -o json 2>/dev/null || true)"

if [[ -n "${EXISTING_SET}" && "${EXISTING_SET}" != "null" ]]; then
  echo "  Attribute set '${ATTRIBUTE_SET_NAME}' already exists — reusing"
else
  echo "  Creating attribute set '${ATTRIBUTE_SET_NAME}'..."
  az rest --method POST \
    --url "${GRAPH_BASE}/directory/attributeSets" \
    --headers "Content-Type=application/json" \
    --body "$(cat <<EOF
{
  "id": "${ATTRIBUTE_SET_NAME}",
  "description": "kars-managed governance attributes for autonomous agent identities. Targets Conditional Access policies via attribute-driven filters.",
  "maxAttributesPerSet": 25
}
EOF
)" \
    -o none
fi

# Helper: idempotently create an attribute. Skips when the attribute
# already exists under the set.
create_attr() {
  local attr_name="$1"
  local description="$2"
  local use_predefined="$3"   # "true" | "false"
  local allowed_values_json="$4"  # JSON array string or empty

  local id="${ATTRIBUTE_SET_NAME}_${attr_name}"
  local existing
  existing="$(az rest --method GET \
    --url "${GRAPH_BASE}/directory/customSecurityAttributeDefinitions/${id}" \
    -o json 2>/dev/null || true)"
  if [[ -n "${existing}" && "${existing}" != "null" ]]; then
    echo "  Attribute '${id}' already exists — reusing"
    return 0
  fi

  echo "  Creating attribute '${id}'..."
  local body
  body="$(jq -n \
    --arg setname "${ATTRIBUTE_SET_NAME}" \
    --arg name "${attr_name}" \
    --arg desc "${description}" \
    --argjson searchable "${SEARCHABLE}" \
    --argjson use_predef "${use_predefined}" \
    --argjson allowed "${allowed_values_json:-[]}" \
    '{
       attributeSet: $setname,
       name: $name,
       description: $desc,
       type: "String",
       status: "Available",
       isCollection: false,
       isSearchable: $searchable,
       usePreDefinedValuesOnly: $use_predef,
       allowedValues: $allowed
     }')"
  az rest --method POST \
    --url "${GRAPH_BASE}/directory/customSecurityAttributeDefinitions" \
    --headers "Content-Type=application/json" \
    --body "${body}" \
    -o none
}

# Step 2: create the four well-known kars attributes.
create_attr "AgentClassification" \
  "Trust tier of the agent. Drives default-deny vs. default-allow Conditional Access targeting." \
  "true" \
  '[{"id":"Standard","isActive":true},{"id":"Restricted","isActive":true},{"id":"Confidential","isActive":true}]'

create_attr "DataSensitivity" \
  "Sensitivity classification of data the agent processes. Maps to Microsoft Information Protection labels in mature deployments." \
  "true" \
  '[{"id":"Public","isActive":true},{"id":"Internal","isActive":true},{"id":"Confidential","isActive":true}]'

create_attr "ProductOwner" \
  "Entra user object ID of the human accountable for this agent. Surfaced in audit + incident response." \
  "false" \
  '[]'

create_attr "ManagedBy" \
  "Provisioning system tag. kars-controller writes 'kars-controller' here on every agent identity it manages." \
  "false" \
  '[]'

echo
echo "Tenant attribute set ready: ${ATTRIBUTE_SET_NAME}"
echo
echo "Next steps:"
echo "  1. Reference these in KarsSandbox.spec.meshAuth.customSecurityAttributes (see docs)"
echo "  2. Apply the baseline CA policy:"
echo "       az deployment tenant create \\"
echo "         --location <region> \\"
echo "         --template-file deploy/bicep/standalone/conditional-access-baseline.bicep \\"
echo "         --parameters attributeSetName=${ATTRIBUTE_SET_NAME}"
