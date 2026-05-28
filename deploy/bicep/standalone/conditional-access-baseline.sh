#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# kars Conditional Access baseline for autonomous agent identities.
#
# Applies the recommended baseline CA policy from Microsoft's
# `policy-autonomous-agents` template
# (https://learn.microsoft.com/entra/agent-id/concept-agent-id-design-patterns).
#
# Policy semantics:
#   * Targets ONLY agent identities tagged with the kars
#     `AgentGovernance.ManagedBy=kars-controller` custom security
#     attribute. Apply `custom-security-attributes.sh` first.
#   * Blocks sign-ins where the risk level meets the configured
#     threshold (default: `high`).
#   * Initial state: `enabledForReportingButNotEnforced` — collects
#     telemetry without blocking. Operators flip to `enabled` after
#     7 days of clean report-mode telemetry.
#
# Required role: `Conditional Access Administrator` or `Security
# Administrator` on the tenant.
#
# Why a shell script? The Microsoft.Graph Bicep extension (v1.0) does
# not yet ship typed declarations for `conditionalAccessPolicies`.
# This script uses `az rest` against the Graph beta endpoint, which
# is the canonical mechanism Microsoft documents for CA management.
#
# Usage:
#   ./conditional-access-baseline.sh
#   POLICY_NAME=kars-baseline POLICY_STATE=enabled ./conditional-access-baseline.sh
#   SIGN_IN_RISK_LEVELS="medium,high" ./conditional-access-baseline.sh
#
# Idempotent: re-runs upsert via PATCH on the existing policy id.

set -euo pipefail

POLICY_NAME="${POLICY_NAME:-kars-autonomous-agents-baseline}"
POLICY_STATE="${POLICY_STATE:-enabledForReportingButNotEnforced}"
ATTRIBUTE_SET_NAME="${ATTRIBUTE_SET_NAME:-AgentGovernance}"
SIGN_IN_RISK_LEVELS="${SIGN_IN_RISK_LEVELS:-high}"

GRAPH_BASE="https://graph.microsoft.com/beta"

case "${POLICY_STATE}" in
  enabled|enabledForReportingButNotEnforced|disabled) ;;
  *)
    echo "ERROR: POLICY_STATE must be enabled|enabledForReportingButNotEnforced|disabled" >&2
    exit 2
    ;;
esac

if ! command -v az >/dev/null 2>&1 || ! command -v jq >/dev/null 2>&1; then
  echo "ERROR: az + jq required on PATH" >&2
  exit 1
fi
if ! az account show -o none 2>/dev/null; then
  echo "ERROR: run 'az login' first" >&2
  exit 1
fi

# Convert comma-separated list to JSON array.
RISK_LEVELS_JSON="$(echo "${SIGN_IN_RISK_LEVELS}" | tr ',' '\n' | jq -R 'select(length>0)' | jq -s .)"

POLICY_BODY="$(jq -n \
  --arg name "${POLICY_NAME}" \
  --arg state "${POLICY_STATE}" \
  --arg setname "${ATTRIBUTE_SET_NAME}" \
  --argjson risk "${RISK_LEVELS_JSON}" \
  '{
     displayName: $name,
     state: $state,
     conditions: {
       applications: {
         includeApplications: ["All"],
         applicationFilter: {
           mode: "include",
           rule: ("CustomSecurityAttribute." + $setname + ".ManagedBy -eq \"kars-controller\"")
         }
       },
       clientAppTypes: ["all"],
       signInRiskLevels: $risk
     },
     grantControls: {
       operator: "OR",
       builtInControls: ["block"]
     }
   }')"

# Find existing policy by displayName.
EXISTING_ID="$(az rest --method GET \
  --url "${GRAPH_BASE}/identity/conditionalAccess/policies?\$select=id,displayName&\$filter=displayName%20eq%20'${POLICY_NAME}'" \
  -o json 2>/dev/null \
  | jq -r '.value[0].id // empty')"

if [[ -n "${EXISTING_ID}" ]]; then
  echo "Updating existing CA policy '${POLICY_NAME}' (id=${EXISTING_ID})..."
  az rest --method PATCH \
    --url "${GRAPH_BASE}/identity/conditionalAccess/policies/${EXISTING_ID}" \
    --headers "Content-Type=application/json" \
    --body "${POLICY_BODY}" \
    -o none
  POLICY_ID="${EXISTING_ID}"
else
  echo "Creating CA policy '${POLICY_NAME}'..."
  POLICY_ID="$(az rest --method POST \
    --url "${GRAPH_BASE}/identity/conditionalAccess/policies" \
    --headers "Content-Type=application/json" \
    --body "${POLICY_BODY}" \
    -o json | jq -r '.id')"
fi

echo
echo "CA policy ready:"
echo "  id    = ${POLICY_ID}"
echo "  name  = ${POLICY_NAME}"
echo "  state = ${POLICY_STATE}"
echo "  risk  = ${SIGN_IN_RISK_LEVELS}"
echo "  set   = ${ATTRIBUTE_SET_NAME}"
echo
echo "Verify in the Entra portal:"
echo "  https://portal.azure.com/#blade/Microsoft_AAD_ConditionalAccess/PolicyBlade/policyId/${POLICY_ID}"
