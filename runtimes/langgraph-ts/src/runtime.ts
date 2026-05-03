// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Bootstrap entrypoint for the LangGraph (TypeScript) adapter.
 *
 * Called from `sandbox-images/langgraph-ts/entrypoint.sh` immediately
 * before the user's graph code runs. Idempotent — guarded by
 * `__AZURECLAW_RUNTIME_INITIALIZED__` so re-imports during user code
 * or tests are no-ops.
 *
 * LangGraph TS is provider-agnostic, but every realistic graph
 * invokes one or more LangChain.js model factories (`ChatOpenAI`,
 * `AzureChatOpenAI`, `ChatAnthropic`, ...). Those factories read
 * provider base URLs and API keys from the process env at
 * construction time. The adapter pins each known provider base URL
 * to the router sidecar's matching proxy endpoint so model calls
 * cannot egress directly. The router enforces governance, content
 * safety, and AAD attestation (no API keys in the pod).
 *
 * For each provider we set the API-key env to a sentinel value
 * (`router-managed`); the router strips and substitutes its own
 * AAD-attested credential on egress. The egress-guard iptables init
 * container drops UID-1000 packets to non-loopback / non-DNS
 * targets, so even a leaked base-url cannot reach the public
 * provider endpoint.
 */

import { initTelemetry } from './otel';

export const ENV_INITIALIZED = '__AZURECLAW_RUNTIME_INITIALIZED__';
export const ROUTER_MANAGED_KEY_SENTINEL = 'router-managed';
export const SERVICE_NAME = 'azureclaw-runtime-langgraph-ts';

interface ProviderPin {
  baseUrlEnv: string;
  defaultUrl: string;
  apiKeyEnv: string | null;
}

export const PROVIDER_BASE_URLS: ProviderPin[] = [
  {
    baseUrlEnv: 'OPENAI_BASE_URL',
    defaultUrl: 'http://127.0.0.1:8443/openai/v1',
    apiKeyEnv: 'OPENAI_API_KEY',
  },
  {
    baseUrlEnv: 'AZURE_OPENAI_ENDPOINT',
    defaultUrl: 'http://127.0.0.1:8443/azure-openai',
    apiKeyEnv: 'AZURE_OPENAI_API_KEY',
  },
  {
    baseUrlEnv: 'ANTHROPIC_BASE_URL',
    defaultUrl: 'http://127.0.0.1:8443/anthropic/v1',
    apiKeyEnv: 'ANTHROPIC_API_KEY',
  },
];

function setIfUnset(env: string, value: string): void {
  if (process.env[env] === undefined || process.env[env] === '') {
    process.env[env] = value;
  }
}

function installSignalHandlers(): void {
  const handler = (signal: NodeJS.Signals) => {
    // eslint-disable-next-line no-console
    console.error(
      `[azureclaw-runtime-langgraph-ts] received ${signal} — exiting`,
    );
    // Allow batched span exporters a brief drain window.
    setTimeout(() => process.exit(0), 50).unref();
  };
  for (const sig of ['SIGTERM', 'SIGINT'] as const) {
    try {
      process.on(sig, handler);
    } catch {
      // Some environments (workers) refuse signal hooks — non-fatal.
    }
  }
}

export interface BootstrapOptions {
  serviceName?: string;
  serviceVersion?: string;
}

/**
 * Idempotently initialize the in-pod adapter.
 *
 * 1. Skip if `__AZURECLAW_RUNTIME_INITIALIZED__` is already set.
 * 2. Pin each known provider base URL to the router sidecar so
 *    LangChain factories cannot reach the public model endpoints
 *    directly (egress-guard would drop the packet anyway).
 * 3. Set each provider API-key env to a sentinel; the router
 *    substitutes the real credential on egress.
 * 4. Initialize OTel.
 * 5. Install signal handlers for graceful shutdown.
 * 6. Mark the env so subsequent imports are no-ops.
 */
export async function bootstrap(
  opts: BootstrapOptions = {},
): Promise<void> {
  if (process.env[ENV_INITIALIZED] === '1') {
    return;
  }

  for (const pin of PROVIDER_BASE_URLS) {
    setIfUnset(pin.baseUrlEnv, pin.defaultUrl);
    if (pin.apiKeyEnv) {
      setIfUnset(pin.apiKeyEnv, ROUTER_MANAGED_KEY_SENTINEL);
    }
  }

  await initTelemetry({
    serviceName: opts.serviceName ?? SERVICE_NAME,
    serviceVersion: opts.serviceVersion,
  });

  installSignalHandlers();

  process.env[ENV_INITIALIZED] = '1';
  // eslint-disable-next-line no-console
  console.error(
    '[azureclaw-runtime-langgraph-ts] bootstrapped: providers pinned to router',
  );
}
