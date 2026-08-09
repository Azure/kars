#!/bin/bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
DOCKERFILE="$SCRIPT_DIR/Dockerfile"

grep -Fq '"hermes-agent[feishu]==${HERMES_VERSION}"' "$DOCKERFILE"
grep -Fq 'patch-hermes-feishu-policy.py' "$DOCKERFILE"
grep -Fq 'import lark_oapi, qrcode' "$DOCKERFILE"
grep -Fq 'kars-channel-feishu-ready' "$DOCKERFILE"
grep -Fq '"ws" / "client.py"' "$SCRIPT_DIR/patch-hermes-feishu-policy.py"
grep -Fq '_KARS_READY_PATH.touch' "$SCRIPT_DIR/patch-hermes-feishu-policy.py"
grep -Fq '_adapter_dm_policy' "$SCRIPT_DIR/patch-hermes-feishu-policy.py"
grep -Fq 'pairing_store.is_approved' "$SCRIPT_DIR/patch-hermes-feishu-policy.py"

READY_MARKER=$(mktemp)
rm -f "$READY_MARKER"
KARS_FEISHU_READY_PATH="$READY_MARKER" bash "$SCRIPT_DIR/kars-channel-feishu-ready" 2>/dev/null && {
  echo "expected Hermes readiness to fail without its marker" >&2
  exit 1
}
touch "$READY_MARKER"
KARS_FEISHU_READY_PATH="$READY_MARKER" bash "$SCRIPT_DIR/kars-channel-feishu-ready"
rm -f "$READY_MARKER"

# shellcheck source=entrypoint.sh
source "$SCRIPT_DIR/entrypoint.sh"

READY_MARKER=$(mktemp)
KARS_FEISHU_READY_PATH="$READY_MARKER" clear_feishu_readiness
if [ -e "$READY_MARKER" ]; then
  echo "expected Hermes entrypoint startup to clear a stale readiness marker" >&2
  exit 1
fi

reset_config() {
  unset FEISHU_APP_ID FEISHU_APP_SECRET FEISHU_DOMAIN FEISHU_CONNECTION_MODE
  unset FEISHU_DM_POLICY FEISHU_ALLOW_FROM FEISHU_GROUP_POLICY
  unset FEISHU_GROUP_ALLOW_FROM FEISHU_REQUIRE_MENTION
}

reset_config
export FEISHU_APP_ID='cli_test'
export FEISHU_APP_SECRET='secret'
export FEISHU_DOMAIN='feishu'
export FEISHU_CONNECTION_MODE='websocket'
export FEISHU_DM_POLICY='pairing'
export FEISHU_GROUP_POLICY='allowlist'
export FEISHU_GROUP_ALLOW_FROM='oc_group1,oc_group2'
export FEISHU_REQUIRE_MENTION='true'
CONFIG=$(render_feishu_platform_config)
printf '%s\n' "$CONFIG" | grep -Fq 'dm_policy: "pairing"'
printf '%s\n' "$CONFIG" | grep -Fq 'default_group_policy: "disabled"'
printf '%s\n' "$CONFIG" | grep -Fq '"oc_group1":'
printf '%s\n' "$CONFIG" | grep -Fq '"oc_group2":'
printf '%s\n' "$CONFIG" | grep -Fq 'require_mention: true'

reset_config
export FEISHU_APP_ID='cli_test'
export FEISHU_APP_SECRET='secret'
export FEISHU_CONNECTION_MODE='websocket'
export FEISHU_DM_POLICY='allowlist'
export FEISHU_ALLOW_FROM='ou_user1,ou_user2'
CONFIG=$(render_feishu_platform_config)
printf '%s\n' "$CONFIG" | grep -Fq 'dm_allow_from: ["ou_user1","ou_user2"]'

reset_config
export FEISHU_APP_SECRET='secret'
export FEISHU_CONNECTION_MODE='websocket'
if (validate_feishu_channel >/dev/null 2>&1); then
  echo "expected partial Feishu credentials to fail" >&2
  exit 1
fi

reset_config
export FEISHU_APP_ID='cli_stale'
export FEISHU_APP_SECRET='stale-secret'
if [ -n "$(render_feishu_platform_config)" ]; then
  echo "expected stale Feishu credentials without typed policy to stay disabled" >&2
  exit 1
fi

reset_config
export FEISHU_APP_ID='cli_test'
export FEISHU_APP_SECRET='secret'
export FEISHU_CONNECTION_MODE='webhook'
if (validate_feishu_channel >/dev/null 2>&1); then
  echo "expected webhook mode to fail" >&2
  exit 1
fi

echo "Hermes Feishu channel tests passed"