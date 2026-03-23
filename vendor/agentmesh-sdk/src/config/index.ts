/**
 * Configuration module for AgentMesh SDK.
 */

// Re-export policy types
export {
  Tier,
  TierLevel,
  getTierLevel,
  Policy,
  Config,
} from './policy';

export type {
  PolicyOptions,
  KnockContext,
  PolicyResult,
  ConfigOptions,
} from './policy';

// Re-export file config loader
export {
  FileConfigLoader,
  ConfigError,
  createFileConfigLoader,
} from './file-config';

export type {
  FileConfigOptions,
  PersistedSessionState,
  FileConfigEventType,
} from './file-config';
