#!/bin/bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BASE_DOCKERFILE="$SCRIPT_DIR/Dockerfile.base"
RUNTIME_DOCKERFILE="$SCRIPT_DIR/Dockerfile"
TEST_ROOT=$(mktemp -d)
trap 'rm -rf "$TEST_ROOT"' EXIT

grep -Fq 'ARG OPENCLAW_VERSION=2026.5.27' "$BASE_DOCKERFILE"
grep -Fq 'plugins install "@openclaw/feishu@${OPENCLAW_VERSION}" --pin' "$BASE_DOCKERFILE"
grep -Fq 'tdnf install -y tar gzip ca-certificates curl git jq' "$BASE_DOCKERFILE"
grep -Fq 'COPY --from=builder /opt/openclaw-feishu-stage /opt/openclaw-feishu-stage' "$BASE_DOCKERFILE"
grep -Fq 'patch-feishu-proxy.cjs' "$BASE_DOCKERFILE"
grep -Fq 'patch-feishu-proxy.cjs' "$RUNTIME_DOCKERFILE"
grep -Fq 'r.httpsAgent = agent' "$SCRIPT_DIR/patch-feishu-proxy.cjs"
grep -Fq 'sanitizeFeishuAxiosError' "$SCRIPT_DIR/patch-feishu-proxy.cjs"
grep -Fq '/opt/openclaw-feishu-stage' "$SCRIPT_DIR/entrypoint.sh"
grep -Fq 'openclaw plugins list' "$BASE_DOCKERFILE"
grep -Fq 'kars-channel-feishu-ready' "$RUNTIME_DOCKERFILE"

mkdir -p "$TEST_ROOT/bin"
cat > "$TEST_ROOT/bin/openclaw" <<'EOF'
#!/bin/bash
printf '%s' "$OPENCLAW_TEST_STATUS"
EOF
chmod +x "$TEST_ROOT/bin/openclaw"
export PATH="$TEST_ROOT/bin:$PATH"
export OPENCLAW_TEST_STATUS='{"gatewayReachable":true,"channelAccounts":{"feishu":[{"enabled":true,"configured":true,"running":true,"lastError":null}]}}'
bash "$SCRIPT_DIR/kars-channel-feishu-ready"
export OPENCLAW_TEST_STATUS='{"channelAccounts":{"feishu":[{"enabled":true,"configured":true,"running":true,"lastError":null}]}}'
if bash "$SCRIPT_DIR/kars-channel-feishu-ready"; then
  echo "expected missing gateway reachability to fail closed" >&2
  exit 1
fi
export OPENCLAW_TEST_STATUS='{"gatewayReachable":false,"configOnly":true,"configuredChannels":["feishu"]}'
if bash "$SCRIPT_DIR/kars-channel-feishu-ready"; then
  echo "expected config-only OpenClaw status to be unready" >&2
  exit 1
fi

node - "$SCRIPT_DIR/patch-feishu-proxy.cjs" <<'EOF'
const { sanitizeFeishuAxiosError } = require(process.argv[2]);
const sentinels = ["secret-value", "Bearer credential", "cli_private", "event-body"];
const error = new Error(sentinels[0]);
error.code = "ECONNRESET";
error.config = {
  url: `https://example.invalid/${sentinels[2]}`,
  headers: { Authorization: sentinels[1] },
  data: sentinels[3],
};
error.response = { status: 403, data: sentinels[3], headers: { cookie: sentinels[0] } };
error.cause = new Error(sentinels[0]);
const serialized = JSON.stringify(sanitizeFeishuAxiosError(error))
  + String(sanitizeFeishuAxiosError(error).stack);
if (sentinels.some((sentinel) => serialized.includes(sentinel))) {
  throw new Error("sanitized Feishu Axios error retained sensitive fields");
}
EOF
export OPENCLAW_TEST_STATUS='{"gatewayReachable":true,"channelAccounts":{"feishu":[{"enabled":true,"configured":true,"running":true,"lastError":"connection failed"}]}}'
if bash "$SCRIPT_DIR/kars-channel-feishu-ready"; then
  echo "expected an OpenClaw account with lastError to be unready" >&2
  exit 1
fi

# shellcheck source=entrypoint.sh
source "$SCRIPT_DIR/entrypoint.sh"

mkdir -p "$TEST_ROOT/plugin-stage/npm/node_modules/@openclaw/feishu"
mkdir -p "$TEST_ROOT/plugin-stage/plugins"
printf '{"plugins":{"feishu":{"source":"npm"}}}' > "$TEST_ROOT/plugin-stage/plugins/installs.json"
export KARS_FEISHU_PLUGIN_STAGE="$TEST_ROOT/plugin-stage"
export OPENCLAW_DIR="$TEST_ROOT/openclaw-state"

reset_config() {
  PLUGINS_LIST='"kars"'
  PLUGINS_ENTRIES='"kars": { "enabled": true }'
  CHANNELS_CONFIG=""
  unset FEISHU_APP_ID FEISHU_APP_SECRET FEISHU_DOMAIN FEISHU_CONNECTION_MODE
  unset FEISHU_DM_POLICY FEISHU_ALLOW_FROM FEISHU_GROUP_POLICY
  unset FEISHU_GROUP_ALLOW_FROM FEISHU_REQUIRE_MENTION
}

reset_config
export FEISHU_APP_ID='cli_test'
export FEISHU_APP_SECRET='secret-with-"quote'
export FEISHU_DOMAIN='feishu'
export FEISHU_CONNECTION_MODE='websocket'
export FEISHU_DM_POLICY='pairing'
export FEISHU_ALLOW_FROM='ou_user1,ou_user2'
export FEISHU_GROUP_POLICY='allowlist'
export FEISHU_GROUP_ALLOW_FROM='oc_group1,oc_group2'
export FEISHU_REQUIRE_MENTION='true'
append_feishu_channel_config

CONFIG=$(printf '{"plugins":{"allow":[%s],"entries":{%s}},"channels":{%s}}' \
  "$PLUGINS_LIST" "$PLUGINS_ENTRIES" "$CHANNELS_CONFIG")
printf '%s' "$CONFIG" | jq -e '
  .plugins.allow == ["kars", "feishu"] and
  .plugins.entries.feishu.enabled == true and
  .channels.feishu.appId == "cli_test" and
  .channels.feishu.appSecret == "secret-with-\"quote" and
  .channels.feishu.domain == "feishu" and
  .channels.feishu.connectionMode == "websocket" and
  .channels.feishu.dmPolicy == "pairing" and
  .channels.feishu.allowFrom == ["ou_user1", "ou_user2"] and
  .channels.feishu.groupPolicy == "allowlist" and
  .channels.feishu.groupAllowFrom == ["oc_group1", "oc_group2"] and
  .channels.feishu.requireMention == true
' >/dev/null

reset_config
export FEISHU_APP_ID='cli_partial'
export FEISHU_CONNECTION_MODE='websocket'
if (append_feishu_channel_config >/dev/null 2>&1); then
  echo "expected partial Feishu credentials to fail" >&2
  exit 1
fi

reset_config
export FEISHU_APP_ID='cli_stale'
export FEISHU_APP_SECRET='stale-secret'
append_feishu_channel_config
if [ -n "$CHANNELS_CONFIG" ]; then
  echo "expected stale Feishu credentials without typed policy to stay disabled" >&2
  exit 1
fi

reset_config
export FEISHU_APP_ID='cli_test'
export FEISHU_APP_SECRET='secret'
export FEISHU_CONNECTION_MODE='websocket'
export KARS_FEISHU_PLUGIN_STAGE="$TEST_ROOT/missing-plugins"
if (append_feishu_channel_config >/dev/null 2>&1); then
  echo "expected a missing Feishu plugin to fail" >&2
  exit 1
fi

echo "OpenClaw Feishu channel tests passed"