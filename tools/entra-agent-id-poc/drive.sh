#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# POC driver — deploys the Bicep template + measures wall-clock at
# each phase, then tears down. Lets us answer: is per-sandbox Entra
# Agent ID provisioning fast enough to inline in `kars up`?
#
# Required privileges (deployment principal):
#   • Application Administrator (Entra role)   — minimal viable
#   • Cloud Application Administrator          — equivalent
#   • Global Administrator                     — overkill
#   • Custom role with
#       microsoft.directory/applications/createAsOwner
#       microsoft.directory/applications/standard/read
#       microsoft.directory/applications/credentials/update
#       microsoft.directory/servicePrincipals/createAsOwner
#
# To check what you have:
#   az rest --method get \
#     --uri "https://graph.microsoft.com/v1.0/me/memberOf" \
#     --query "value[?'@odata.type'=='#microsoft.graph.directoryRole'].displayName"
#
# Usage:
#   ./drive.sh                 # full provision + measure + cleanup
#   ./drive.sh --keep          # leave the Entra app in place for inspection
#   ./drive.sh --probe         # just probe what permissions you have, exit
set -euo pipefail

KEEP=0
PROBE=0
for arg in "$@"; do
    case "$arg" in
        --keep)  KEEP=1 ;;
        --probe) PROBE=1 ;;
        *) echo "unknown arg: $arg" >&2; exit 2 ;;
    esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUN_ID="poc$(date +%Y%m%d%H%M%S)"
DEPLOY_NAME="kars-entra-agentid-${RUN_ID}"

red()   { printf "\033[31m%s\033[0m\n" "$*"; }
green() { printf "\033[32m%s\033[0m\n" "$*"; }
yellow(){ printf "\033[33m%s\033[0m\n" "$*"; }
blue()  { printf "\033[34m%s\033[0m\n" "$*"; }
bold()  { printf "\033[1m%s\033[0m\n" "$*"; }

bold "═══ Entra Agent ID provisioning POC ($(date)) ═══"
echo

# ── Phase 0: preflight ───────────────────────────────────────────
ACCOUNT_JSON=$(az account show -o json 2>/dev/null) || {
    red "ERR: az CLI not signed in."
    yellow "Fix: az login --tenant <tenant> --scope https://graph.microsoft.com//.default"
    exit 1
}
TENANT_ID=$(echo "$ACCOUNT_JSON" | jq -r .tenantId)
SUB_ID=$(echo "$ACCOUNT_JSON" | jq -r .id)
USER_NAME=$(echo "$ACCOUNT_JSON" | jq -r '.user.name')
yellow "  tenant:       $TENANT_ID"
yellow "  subscription: $SUB_ID"
yellow "  user:         $USER_NAME"
yellow "  run ID:       $RUN_ID"
echo

# Probe — what Entra roles does the current principal hold?
yellow "  Probing role assignments via Graph (read-only)..."
ROLES_JSON=$(az rest --method get \
    --uri "https://graph.microsoft.com/v1.0/me/memberOf?\$top=999" \
    --query "value[?'@odata.type'=='#microsoft.graph.directoryRole'].{role:displayName,id:roleTemplateId}" \
    -o json 2>/dev/null) || ROLES_JSON='[]'

if [ "$ROLES_JSON" = "[]" ] || [ -z "$ROLES_JSON" ]; then
    red "  ⚠ No Graph-readable directoryRole assignments found (or token blocked)."
    red "    The deployment may still succeed if your principal has a custom"
    red "    role with applications/createAsOwner — try anyway."
else
    echo "$ROLES_JSON" | jq -r '.[] | "  • " + .role'
fi

# Are any of the matching roles present?
HAS_ROLE=0
for needle in "Application Administrator" "Cloud Application Administrator" "Global Administrator"; do
    if echo "$ROLES_JSON" | grep -q "$needle"; then
        green "  ✓ has '$needle' — provisioning will succeed"
        HAS_ROLE=1
        break
    fi
done
if [ "$HAS_ROLE" -eq 0 ]; then
    yellow "  ⚠ none of {Application Administrator, Cloud App Admin, Global Admin}"
    yellow "    in role list — deployment may need elevation. Continuing anyway."
fi
echo

if [ "$PROBE" -eq 1 ]; then
    bold "Probe complete (--probe set). Exiting without deploy."
    exit 0
fi

# ── Phase 1: Bicep build (compile-only check) ─────────────────────
bold "Phase 1: Bicep compile"
START=$(date +%s.%N)
az bicep build --file "$SCRIPT_DIR/main.bicep" --outfile "$SCRIPT_DIR/main.json" 2>&1 | tail -5
END=$(date +%s.%N)
T_BUILD=$(awk "BEGIN { printf \"%.2f\", $END - $START }")
green "  ✓ compiled in ${T_BUILD}s"
echo

# ── Phase 2: deploy ──────────────────────────────────────────────
bold "Phase 2: deploy (creates app + SP + fedcred via Bicep Microsoft.Graph extension)"
START=$(date +%s.%N)
DEPLOY_JSON=$(az deployment sub create \
    --location "eastus" \
    --name "$DEPLOY_NAME" \
    --template-file "$SCRIPT_DIR/main.bicep" \
    --parameters runId="$RUN_ID" \
                 serviceManagementReference="${KARS_SERVICETREE_GUID:-}" \
                 fedCredIssuer="${KARS_OIDC_ISSUER:-https://oidc.example.test/poc-${RUN_ID}}" \
    -o json 2>&1) || {
    END=$(date +%s.%N)
    T_DEPLOY=$(awk "BEGIN { printf \"%.2f\", $END - $START }")
    red "  ✗ deploy failed after ${T_DEPLOY}s"
    echo "$DEPLOY_JSON" | tail -25
    exit 1
}
END=$(date +%s.%N)
T_DEPLOY=$(awk "BEGIN { printf \"%.2f\", $END - $START }")
green "  ✓ deployed in ${T_DEPLOY}s"

APP_ID=$(echo "$DEPLOY_JSON"        | jq -r '.properties.outputs.appId.value')
APP_OBJECT_ID=$(echo "$DEPLOY_JSON" | jq -r '.properties.outputs.appObjectId.value')
SP_OBJECT_ID=$(echo "$DEPLOY_JSON"  | jq -r '.properties.outputs.spObjectId.value')
UNIQUE_NAME=$(echo "$DEPLOY_JSON"   | jq -r '.properties.outputs.uniqueName.value')
yellow "  appId:        $APP_ID"
yellow "  appObjectId:  $APP_OBJECT_ID"
yellow "  spObjectId:   $SP_OBJECT_ID"
yellow "  uniqueName:   $UNIQUE_NAME"
echo

# ── Phase 3: acquire access token (proves the agent identity works) ─
bold "Phase 3: token acquisition"
START=$(date +%s.%N)
TOKEN=$(az account get-access-token --resource "$APP_ID" --query accessToken -o tsv 2>/dev/null) || TOKEN=""
END=$(date +%s.%N)
T_TOKEN=$(awk "BEGIN { printf \"%.2f\", $END - $START }")
if [ -n "$TOKEN" ]; then
    green "  ✓ token acquired in ${T_TOKEN}s  (length=${#TOKEN})"
    PAYLOAD=$(echo "$TOKEN" | cut -d'.' -f2)
    PADDED="${PAYLOAD}$(printf '%*s' $((4 - ${#PAYLOAD} % 4)) | tr ' ' '=')"
    echo "$PADDED" | base64 -d 2>/dev/null | python3 -c "
import sys, json
d = json.loads(sys.stdin.read())
for k in ['iss','aud','tid','oid','sub','appid','idtyp','upn']:
    v = d.get(k, '<absent>')
    print(f'  {k}: {v}')
" || true
else
    red "  ⚠ token acquisition skipped or blocked"
fi
echo

# ── Phase 4: cleanup ─────────────────────────────────────────────
if [ "$KEEP" -eq 1 ]; then
    yellow "Phase 4: cleanup SKIPPED (--keep set). To delete later:"
    yellow "  az ad app delete --id $APP_OBJECT_ID"
    echo
else
    bold "Phase 4: cleanup"
    START=$(date +%s.%N)
    az deployment sub delete --name "$DEPLOY_NAME" 2>/dev/null || true
    # Bicep tenant-deployment delete only removes the deployment record;
    # the underlying Graph objects must be deleted explicitly.
    az ad app delete --id "$APP_OBJECT_ID" 2>&1 | tail -3
    END=$(date +%s.%N)
    T_CLEANUP=$(awk "BEGIN { printf \"%.2f\", $END - $START }")
    green "  ✓ cleanup in ${T_CLEANUP}s"
    echo
fi

# ── Summary ──────────────────────────────────────────────────────
bold "═══ Summary ═══"
echo
printf "  %-50s %s\n" "Phase 1 — Bicep compile:"      "${T_BUILD}s"
printf "  %-50s %s\n" "Phase 2 — Entra provision (app+SP+fedcred):" "${T_DEPLOY}s"
printf "  %-50s %s\n" "Phase 3 — token acquisition:"  "${T_TOKEN}s"
[ "$KEEP" -eq 0 ] && printf "  %-50s %s\n" "Phase 4 — cleanup:" "${T_CLEANUP:-N/A}s"
echo
yellow "kars integration implications:"
yellow "  • Per-sandbox cost = Phase 2 minus the constant overhead"
yellow "    (app+SP are usually one-time; fedcred repeats per sandbox)"
yellow "  • Inline in sandbox-spawn iff Phase 2 < ~3s on cold cache"
yellow "  • Otherwise, controller pre-provisions in fedcred reconciler"
yellow "    (already the case today — see controller/src/fedcred.rs)"
