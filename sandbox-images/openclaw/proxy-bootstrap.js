// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// proxy-bootstrap.js — preloaded via NODE_OPTIONS="--require ..."
// Sets undici's EnvHttpProxyAgent as the global fetch dispatcher so all
// outbound HTTP(S) requests honour HTTPS_PROXY / NO_PROXY env vars.
// This runs before any OpenClaw code, ensuring Telegram polling, model
// pricing, and every other fetch goes through the Kars forward proxy.
'use strict';

if (process.env.HTTPS_PROXY || process.env.HTTP_PROXY) {
  try {
    const undici = require('/usr/local/lib/node_modules/openclaw/node_modules/undici');
    undici.setGlobalDispatcher(new undici.EnvHttpProxyAgent());
  } catch (_) {
    // If undici isn't available (unlikely in OpenClaw sandbox), fall through
    // silently — transparent proxy via iptables is the fallback.
  }
}
