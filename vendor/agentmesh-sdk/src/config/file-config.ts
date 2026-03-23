/**
 * File-based configuration loader for AgentMesh.
 * Handles loading policy from ~/.agentmesh/policy.json and session persistence.
 */

import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { PolicyOptions, Policy } from './index';
import { EventEmitter } from 'events';

/**
 * Configuration error.
 */
export class ConfigError extends Error {
  constructor(message: string, public readonly cause?: Error) {
    super(message);
    this.name = 'ConfigError';
  }
}

/**
 * File configuration options.
 */
export interface FileConfigOptions {
  /** Base directory (default: ~/.agentmesh or AGENTMESH_HOME) */
  baseDir?: string;
  /** Watch for policy file changes */
  watchPolicy?: boolean;
  /** Use file storage (default: true, set false for containers) */
  useFileStorage?: boolean;
  /** Graceful fallback for read-only filesystems */
  gracefulFallback?: boolean;
}

/**
 * Session state for persistence.
 */
export interface PersistedSessionState {
  sessionId: string;
  remoteAmid: string;
  isInitiator: boolean;
  createdAt: string;
  expiresAt: string;
  state: string;
  ratchetState?: string;
}

/**
 * File configuration loader events.
 */
export type FileConfigEventType =
  | 'policy_reloaded'
  | 'policy_reload_failed'
  | 'session_persisted'
  | 'session_restored'
  | 'session_cleanup';

/**
 * File configuration loader.
 * Manages loading and persisting configuration from the filesystem.
 */
export class FileConfigLoader extends EventEmitter {
  private readonly baseDir: string;
  private readonly keysDir: string;
  private readonly sessionsDir: string;
  private readonly policyPath: string;
  private readonly useFileStorage: boolean;
  private readonly gracefulFallback: boolean;
  private policyWatcher: fs.FSWatcher | null = null;
  private currentPolicy: Policy | null = null;

  constructor(options: FileConfigOptions = {}) {
    super();

    this.baseDir = options.baseDir || this.resolveBaseDir();
    this.keysDir = path.join(this.baseDir, 'keys');
    this.sessionsDir = path.join(this.baseDir, 'sessions');
    this.policyPath = path.join(this.baseDir, 'policy.json');
    this.useFileStorage = options.useFileStorage ?? true;
    this.gracefulFallback = options.gracefulFallback ?? true;

    if (this.useFileStorage) {
      this.ensureDirectories();
    }

    if (options.watchPolicy && this.useFileStorage) {
      this.startPolicyWatcher();
    }
  }

  /**
   * Resolve the base directory.
   */
  private resolveBaseDir(): string {
    // Check environment variable first
    if (process.env.AGENTMESH_HOME) {
      return process.env.AGENTMESH_HOME;
    }
    // Default to ~/.agentmesh
    return path.join(os.homedir(), '.agentmesh');
  }

  /**
   * Ensure required directories exist with proper permissions.
   */
  private ensureDirectories(): void {
    try {
      // Create directories with 0700 permissions (owner only)
      const dirs = [this.baseDir, this.keysDir, this.sessionsDir];
      for (const dir of dirs) {
        if (!fs.existsSync(dir)) {
          fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
        }
      }
    } catch (error) {
      if (this.gracefulFallback) {
        console.warn(`Failed to create AgentMesh directories: ${error}`);
      } else {
        throw new ConfigError('Failed to create AgentMesh directories', error as Error);
      }
    }
  }

  /**
   * Get the base directory path.
   */
  getBaseDir(): string {
    return this.baseDir;
  }

  /**
   * Get the keys directory path.
   */
  getKeysDir(): string {
    return this.keysDir;
  }

  /**
   * Get the sessions directory path.
   */
  getSessionsDir(): string {
    return this.sessionsDir;
  }

  /**
   * Load policy from file.
   */
  loadPolicy(policyPath?: string): Policy {
    const filePath = policyPath || this.policyPath;

    try {
      if (!fs.existsSync(filePath)) {
        // Return default policy if file doesn't exist
        this.currentPolicy = new Policy({});
        return this.currentPolicy;
      }

      const content = fs.readFileSync(filePath, 'utf-8');
      const policyData = JSON.parse(content) as PolicyOptions;

      // Validate policy content
      this.validatePolicyContent(policyData);

      this.currentPolicy = new Policy(policyData);
      return this.currentPolicy;
    } catch (error) {
      // Handle graceful fallback first
      if (this.gracefulFallback) {
        console.warn(`Failed to load policy, using defaults: ${error}`);
        this.currentPolicy = new Policy({});
        return this.currentPolicy;
      }
      // Throw specific errors
      if (error instanceof SyntaxError) {
        throw new ConfigError(`Invalid JSON in policy file: ${filePath}`, error);
      }
      if (error instanceof ConfigError) {
        throw error;
      }
      throw new ConfigError(`Failed to load policy from ${filePath}`, error as Error);
    }
  }

  /**
   * Validate policy content.
   */
  private validatePolicyContent(data: unknown): asserts data is PolicyOptions {
    if (typeof data !== 'object' || data === null) {
      throw new ConfigError('Policy must be an object');
    }

    const policy = data as Record<string, unknown>;

    // Validate specific fields if present
    if (policy.minTier !== undefined) {
      const validTiers = ['anonymous', 'verified', 'organization'];
      if (typeof policy.minTier !== 'string' || !validTiers.includes(policy.minTier)) {
        throw new ConfigError('minTier must be "anonymous", "verified", or "organization"');
      }
    }

    if (policy.minReputation !== undefined) {
      if (typeof policy.minReputation !== 'number' || policy.minReputation < 0 || policy.minReputation > 1) {
        throw new ConfigError('minReputation must be a number between 0 and 1');
      }
    }

    if (policy.maxConcurrentSessions !== undefined) {
      if (typeof policy.maxConcurrentSessions !== 'number' || policy.maxConcurrentSessions < 1) {
        throw new ConfigError('maxConcurrentSessions must be a positive number');
      }
    }

    if (policy.blockedAmids !== undefined) {
      if (!Array.isArray(policy.blockedAmids)) {
        throw new ConfigError('blockedAmids must be an array');
      }
    }

    if (policy.allowedAmids !== undefined) {
      if (!Array.isArray(policy.allowedAmids)) {
        throw new ConfigError('allowedAmids must be an array');
      }
    }

    if (policy.acceptedIntents !== undefined) {
      if (!Array.isArray(policy.acceptedIntents)) {
        throw new ConfigError('acceptedIntents must be an array');
      }
    }
  }

  /**
   * Save policy to file.
   */
  savePolicy(policy: PolicyOptions, policyPath?: string): void {
    const filePath = policyPath || this.policyPath;

    try {
      const content = JSON.stringify(policy, null, 2);
      fs.writeFileSync(filePath, content, { mode: 0o600 });
    } catch (error) {
      throw new ConfigError(`Failed to save policy to ${filePath}`, error as Error);
    }
  }

  /**
   * Start watching policy file for changes.
   */
  private startPolicyWatcher(): void {
    if (this.policyWatcher) {
      return;
    }

    try {
      // Ensure policy file exists before watching
      if (!fs.existsSync(this.policyPath)) {
        // Create empty policy file
        this.savePolicy({});
      }

      this.policyWatcher = fs.watch(this.policyPath, (eventType) => {
        if (eventType === 'change') {
          this.reloadPolicy();
        }
      });
    } catch (error) {
      console.warn(`Failed to watch policy file: ${error}`);
    }
  }

  /**
   * Reload policy from file.
   */
  private reloadPolicy(): void {
    try {
      const newPolicy = this.loadPolicy();
      this.currentPolicy = newPolicy;
      this.emit('policy_reloaded', newPolicy);
    } catch (error) {
      this.emit('policy_reload_failed', error);
    }
  }

  /**
   * Get the current policy.
   */
  getPolicy(): Policy | null {
    return this.currentPolicy;
  }

  /**
   * Persist a session to disk.
   */
  persistSession(session: PersistedSessionState): void {
    if (!this.useFileStorage) {
      return;
    }

    try {
      const filePath = path.join(this.sessionsDir, `${session.sessionId}.json`);
      const content = JSON.stringify(session, null, 2);
      fs.writeFileSync(filePath, content, { mode: 0o600 });
      this.emit('session_persisted', session.sessionId);
    } catch (error) {
      if (!this.gracefulFallback) {
        throw new ConfigError(`Failed to persist session ${session.sessionId}`, error as Error);
      }
    }
  }

  /**
   * Restore all sessions from disk.
   */
  restoreSessions(): PersistedSessionState[] {
    if (!this.useFileStorage) {
      return [];
    }

    const sessions: PersistedSessionState[] = [];

    try {
      if (!fs.existsSync(this.sessionsDir)) {
        return [];
      }

      const files = fs.readdirSync(this.sessionsDir);
      const now = new Date();

      for (const file of files) {
        if (!file.endsWith('.json')) {
          continue;
        }

        try {
          const filePath = path.join(this.sessionsDir, file);
          const content = fs.readFileSync(filePath, 'utf-8');
          const session = JSON.parse(content) as PersistedSessionState;

          // Check if session is expired
          const expiresAt = new Date(session.expiresAt);
          if (expiresAt <= now) {
            // Delete expired session
            fs.unlinkSync(filePath);
            continue;
          }

          sessions.push(session);
          this.emit('session_restored', session.sessionId);
        } catch {
          // Skip invalid session files
        }
      }
    } catch (error) {
      if (!this.gracefulFallback) {
        throw new ConfigError('Failed to restore sessions', error as Error);
      }
    }

    return sessions;
  }

  /**
   * Delete a persisted session.
   */
  deleteSession(sessionId: string): void {
    if (!this.useFileStorage) {
      return;
    }

    try {
      const filePath = path.join(this.sessionsDir, `${sessionId}.json`);
      if (fs.existsSync(filePath)) {
        fs.unlinkSync(filePath);
      }
    } catch {
      // Ignore deletion errors
    }
  }

  /**
   * Clean up expired sessions.
   */
  cleanupExpiredSessions(): number {
    if (!this.useFileStorage) {
      return 0;
    }

    let cleaned = 0;

    try {
      if (!fs.existsSync(this.sessionsDir)) {
        return 0;
      }

      const files = fs.readdirSync(this.sessionsDir);
      const now = new Date();

      for (const file of files) {
        if (!file.endsWith('.json')) {
          continue;
        }

        try {
          const filePath = path.join(this.sessionsDir, file);
          const content = fs.readFileSync(filePath, 'utf-8');
          const session = JSON.parse(content) as PersistedSessionState;

          // Check if session is expired
          const expiresAt = new Date(session.expiresAt);
          if (expiresAt <= now) {
            fs.unlinkSync(filePath);
            cleaned++;
          }
        } catch {
          // Skip invalid files
        }
      }

      if (cleaned > 0) {
        this.emit('session_cleanup', cleaned);
      }
    } catch (error) {
      if (!this.gracefulFallback) {
        throw new ConfigError('Failed to cleanup sessions', error as Error);
      }
    }

    return cleaned;
  }

  /**
   * Stop watching policy file.
   */
  stopWatching(): void {
    if (this.policyWatcher) {
      this.policyWatcher.close();
      this.policyWatcher = null;
    }
  }

  /**
   * Close the file config loader.
   */
  close(): void {
    this.stopWatching();
    this.removeAllListeners();
  }
}

/**
 * Create a file config loader with default options.
 */
export function createFileConfigLoader(options?: FileConfigOptions): FileConfigLoader {
  return new FileConfigLoader(options);
}
