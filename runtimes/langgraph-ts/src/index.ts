// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

export const VERSION = '0.1.0';

export {
  bootstrap,
  PROVIDER_BASE_URLS,
  ROUTER_MANAGED_KEY_SENTINEL,
  SERVICE_NAME,
  ENV_INITIALIZED,
} from './runtime';
export type { BootstrapOptions } from './runtime';

export { TokenBroker, getToken, resetDefaultBroker, DEFAULT_SCOPE } from './aad';
export type { CredentialLike, AccessTokenLike } from './aad';

export {
  MeshClient,
  sendMessage,
  receiveMessages,
  resetDefaultClient,
  DEFAULT_RELAY_URL,
  DEFAULT_REGISTRY_URL,
  ENV_AGENT_DID,
  ENV_AGENT_NAME,
} from './mesh';
export type { MeshClientOptions, TaskEnvelope } from './mesh';

export { initTelemetry } from './otel';
export type { InitTelemetryOptions } from './otel';
