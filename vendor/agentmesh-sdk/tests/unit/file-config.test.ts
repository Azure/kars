/**
 * Tests for FileConfigLoader.
 */

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { FileConfigLoader, ConfigError, createFileConfigLoader } from '../../src/config';
import { Policy, Tier } from '../../src/config';

describe('FileConfigLoader', () => {
  let tempDir: string;
  let loader: FileConfigLoader;

  beforeEach(() => {
    // Create a temporary directory for tests
    tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agentmesh-test-'));
  });

  afterEach(() => {
    if (loader) {
      loader.close();
    }
    // Clean up temp directory
    fs.rmSync(tempDir, { recursive: true, force: true });
  });

  describe('constructor', () => {
    it('should use custom base directory', () => {
      loader = new FileConfigLoader({ baseDir: tempDir });
      expect(loader.getBaseDir()).toBe(tempDir);
    });

    it('should create required directories', () => {
      loader = new FileConfigLoader({ baseDir: tempDir });
      expect(fs.existsSync(path.join(tempDir, 'keys'))).toBe(true);
      expect(fs.existsSync(path.join(tempDir, 'sessions'))).toBe(true);
    });

    it('should use AGENTMESH_HOME if set', () => {
      const originalEnv = process.env.AGENTMESH_HOME;
      process.env.AGENTMESH_HOME = tempDir;

      try {
        loader = new FileConfigLoader({});
        expect(loader.getBaseDir()).toBe(tempDir);
      } finally {
        if (originalEnv) {
          process.env.AGENTMESH_HOME = originalEnv;
        } else {
          delete process.env.AGENTMESH_HOME;
        }
      }
    });

    it('should skip directory creation when useFileStorage is false', () => {
      const nonExistentDir = path.join(tempDir, 'non-existent');
      loader = new FileConfigLoader({ baseDir: nonExistentDir, useFileStorage: false });
      expect(fs.existsSync(nonExistentDir)).toBe(false);
    });
  });

  describe('loadPolicy', () => {
    it('should return default policy if file does not exist', () => {
      loader = new FileConfigLoader({ baseDir: tempDir });
      const policy = loader.loadPolicy();
      expect(policy).toBeInstanceOf(Policy);
      expect(policy.minTier).toBe(Tier.ANONYMOUS);
    });

    it('should load policy from file', () => {
      loader = new FileConfigLoader({ baseDir: tempDir });

      // Create policy file
      const policyPath = path.join(tempDir, 'policy.json');
      fs.writeFileSync(policyPath, JSON.stringify({
        minTier: 'verified',
        minReputation: 0.5,
        blockedAmids: ['blocked-amid'],
      }));

      const policy = loader.loadPolicy();
      expect(policy.minTier).toBe(Tier.VERIFIED);
      expect(policy.minReputation).toBe(0.5);
      expect(policy.blockedAmids.has('blocked-amid')).toBe(true);
    });

    it('should throw ConfigError for invalid JSON', () => {
      loader = new FileConfigLoader({ baseDir: tempDir, gracefulFallback: false });

      const policyPath = path.join(tempDir, 'policy.json');
      fs.writeFileSync(policyPath, 'not valid json');

      expect(() => loader.loadPolicy()).toThrow(ConfigError);
    });

    it('should throw ConfigError for invalid minTier', () => {
      loader = new FileConfigLoader({ baseDir: tempDir, gracefulFallback: false });

      const policyPath = path.join(tempDir, 'policy.json');
      fs.writeFileSync(policyPath, JSON.stringify({ minTier: 'invalid_tier' }));

      expect(() => loader.loadPolicy()).toThrow(ConfigError);
    });

    it('should throw ConfigError for invalid minReputation', () => {
      loader = new FileConfigLoader({ baseDir: tempDir, gracefulFallback: false });

      const policyPath = path.join(tempDir, 'policy.json');
      fs.writeFileSync(policyPath, JSON.stringify({ minReputation: 2.0 }));

      expect(() => loader.loadPolicy()).toThrow(ConfigError);
    });

    it('should use graceful fallback on error', () => {
      loader = new FileConfigLoader({ baseDir: tempDir, gracefulFallback: true });

      const policyPath = path.join(tempDir, 'policy.json');
      fs.writeFileSync(policyPath, 'not valid json');

      // Should not throw, should return default policy
      const policy = loader.loadPolicy();
      expect(policy).toBeInstanceOf(Policy);
    });
  });

  describe('savePolicy', () => {
    it('should save policy to file', () => {
      loader = new FileConfigLoader({ baseDir: tempDir });

      loader.savePolicy({
        minTier: Tier.VERIFIED,
        minReputation: 0.7,
      });

      const policyPath = path.join(tempDir, 'policy.json');
      const content = JSON.parse(fs.readFileSync(policyPath, 'utf-8'));
      expect(content.minTier).toBe('verified');
      expect(content.minReputation).toBe(0.7);
    });
  });

  describe('session persistence', () => {
    it('should persist session to file', () => {
      loader = new FileConfigLoader({ baseDir: tempDir });

      const session = {
        sessionId: 'test-session-123',
        remoteAmid: 'remote-amid',
        isInitiator: true,
        createdAt: new Date().toISOString(),
        expiresAt: new Date(Date.now() + 3600000).toISOString(),
        state: 'ESTABLISHED',
      };

      loader.persistSession(session);

      const filePath = path.join(tempDir, 'sessions', 'test-session-123.json');
      expect(fs.existsSync(filePath)).toBe(true);

      const content = JSON.parse(fs.readFileSync(filePath, 'utf-8'));
      expect(content.sessionId).toBe('test-session-123');
    });

    it('should restore sessions from disk', () => {
      loader = new FileConfigLoader({ baseDir: tempDir });

      // Create session file
      const session = {
        sessionId: 'restored-session',
        remoteAmid: 'remote-amid',
        isInitiator: false,
        createdAt: new Date().toISOString(),
        expiresAt: new Date(Date.now() + 3600000).toISOString(),
        state: 'ESTABLISHED',
      };

      const filePath = path.join(tempDir, 'sessions', 'restored-session.json');
      fs.writeFileSync(filePath, JSON.stringify(session));

      const sessions = loader.restoreSessions();
      expect(sessions).toHaveLength(1);
      expect(sessions[0]!.sessionId).toBe('restored-session');
    });

    it('should skip expired sessions during restore', () => {
      loader = new FileConfigLoader({ baseDir: tempDir });

      // Create expired session file
      const session = {
        sessionId: 'expired-session',
        remoteAmid: 'remote-amid',
        isInitiator: false,
        createdAt: new Date(Date.now() - 7200000).toISOString(),
        expiresAt: new Date(Date.now() - 3600000).toISOString(), // Expired 1 hour ago
        state: 'ESTABLISHED',
      };

      const filePath = path.join(tempDir, 'sessions', 'expired-session.json');
      fs.writeFileSync(filePath, JSON.stringify(session));

      const sessions = loader.restoreSessions();
      expect(sessions).toHaveLength(0);
      expect(fs.existsSync(filePath)).toBe(false); // Should be deleted
    });

    it('should delete session file', () => {
      loader = new FileConfigLoader({ baseDir: tempDir });

      const filePath = path.join(tempDir, 'sessions', 'to-delete.json');
      fs.writeFileSync(filePath, JSON.stringify({ sessionId: 'to-delete' }));
      expect(fs.existsSync(filePath)).toBe(true);

      loader.deleteSession('to-delete');
      expect(fs.existsSync(filePath)).toBe(false);
    });

    it('should clean up expired sessions', () => {
      loader = new FileConfigLoader({ baseDir: tempDir });

      // Create one expired and one valid session
      const expired = {
        sessionId: 'expired',
        expiresAt: new Date(Date.now() - 1000).toISOString(),
      };
      const valid = {
        sessionId: 'valid',
        expiresAt: new Date(Date.now() + 3600000).toISOString(),
      };

      fs.writeFileSync(path.join(tempDir, 'sessions', 'expired.json'), JSON.stringify(expired));
      fs.writeFileSync(path.join(tempDir, 'sessions', 'valid.json'), JSON.stringify(valid));

      const cleaned = loader.cleanupExpiredSessions();
      expect(cleaned).toBe(1);
      expect(fs.existsSync(path.join(tempDir, 'sessions', 'expired.json'))).toBe(false);
      expect(fs.existsSync(path.join(tempDir, 'sessions', 'valid.json'))).toBe(true);
    });

    it('should not persist when useFileStorage is false', () => {
      loader = new FileConfigLoader({ baseDir: tempDir, useFileStorage: false });

      loader.persistSession({
        sessionId: 'no-persist',
        remoteAmid: 'remote',
        isInitiator: true,
        createdAt: new Date().toISOString(),
        expiresAt: new Date(Date.now() + 3600000).toISOString(),
        state: 'ESTABLISHED',
      });

      const filePath = path.join(tempDir, 'sessions', 'no-persist.json');
      expect(fs.existsSync(filePath)).toBe(false);
    });
  });

  describe('events', () => {
    it('should emit session_persisted event', () => {
      loader = new FileConfigLoader({ baseDir: tempDir });
      const handler = vi.fn();
      loader.on('session_persisted', handler);

      loader.persistSession({
        sessionId: 'event-test',
        remoteAmid: 'remote',
        isInitiator: true,
        createdAt: new Date().toISOString(),
        expiresAt: new Date(Date.now() + 3600000).toISOString(),
        state: 'ESTABLISHED',
      });

      expect(handler).toHaveBeenCalledWith('event-test');
    });

    it('should emit session_cleanup event', () => {
      loader = new FileConfigLoader({ baseDir: tempDir });
      const handler = vi.fn();
      loader.on('session_cleanup', handler);

      // Create expired session
      fs.writeFileSync(
        path.join(tempDir, 'sessions', 'cleanup-test.json'),
        JSON.stringify({ sessionId: 'cleanup-test', expiresAt: new Date(Date.now() - 1000).toISOString() })
      );

      loader.cleanupExpiredSessions();
      expect(handler).toHaveBeenCalledWith(1);
    });
  });

  describe('createFileConfigLoader', () => {
    it('should create a FileConfigLoader instance', () => {
      loader = createFileConfigLoader({ baseDir: tempDir });
      expect(loader).toBeInstanceOf(FileConfigLoader);
    });
  });
});
