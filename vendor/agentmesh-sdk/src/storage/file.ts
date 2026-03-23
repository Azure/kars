import type { Storage, StorageSetOptions } from './interface';
import { StorageError } from '../errors';
import * as os from 'os';
import * as nodePath from 'path';

/**
 * Get the default AgentMesh storage path.
 * Uses AGENTMESH_HOME environment variable or ~/.agentmesh/
 */
export function getDefaultAgentMeshPath(): string {
  if (process.env.AGENTMESH_HOME) {
    return process.env.AGENTMESH_HOME;
  }
  return nodePath.join(os.homedir(), '.agentmesh');
}

/**
 * Create a FileStorage instance with the default AgentMesh path.
 */
export function createDefaultFileStorage(): FileStorage {
  return new FileStorage(getDefaultAgentMeshPath());
}

/**
 * Filesystem-based storage implementation for Node.js environments.
 * Uses dynamic imports to avoid bundling Node.js-specific modules.
 */
export class FileStorage implements Storage {
  private basePath: string;
  private fs: typeof import('node:fs/promises') | null = null;
  private path: typeof import('node:path') | null = null;

  constructor(basePath: string) {
    this.basePath = basePath;
  }

  private async ensureModules(): Promise<void> {
    if (this.fs === null || this.path === null) {
      try {
        this.fs = await import('node:fs/promises');
        this.path = await import('node:path');
      } catch {
        throw new StorageError(
          'FileStorage requires Node.js environment with fs module',
          'FS_NOT_AVAILABLE'
        );
      }
    }
  }

  private async ensureDir(filePath: string): Promise<void> {
    await this.ensureModules();
    const dir = this.path!.dirname(filePath);
    await this.fs!.mkdir(dir, { recursive: true });
  }

  private resolvePath(key: string): string {
    // Sanitize key to prevent path traversal
    const sanitized = key.replace(/\.\./g, '_').replace(/^\//, '');
    return this.path!.join(this.basePath, sanitized);
  }

  async get(key: string): Promise<Uint8Array | null> {
    await this.ensureModules();
    const filePath = this.resolvePath(key);

    try {
      const data = await this.fs!.readFile(filePath);
      return new Uint8Array(data);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') {
        return null;
      }
      throw new StorageError(`Failed to read file: ${key}`, 'READ_ERROR');
    }
  }

  async set(key: string, value: Uint8Array, options?: StorageSetOptions): Promise<void> {
    await this.ensureModules();
    const filePath = this.resolvePath(key);

    try {
      await this.ensureDir(filePath);

      // Write with restrictive permissions for sensitive data
      await this.fs!.writeFile(filePath, value, { mode: 0o600 });

      // Store metadata in a separate file if provided
      if (options?.metadata) {
        const metaPath = filePath + '.meta.json';
        const metaData = {
          ...options.metadata,
          ...(options.ttl !== undefined && { expiresAt: Date.now() + options.ttl * 1000 }),
        };
        await this.fs!.writeFile(metaPath, JSON.stringify(metaData), { mode: 0o600 });
      }
    } catch (error) {
      throw new StorageError(`Failed to write file: ${key}`, 'WRITE_ERROR');
    }
  }

  async delete(key: string): Promise<void> {
    await this.ensureModules();
    const filePath = this.resolvePath(key);

    try {
      await this.fs!.unlink(filePath);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') {
        throw new StorageError(`Failed to delete file: ${key}`, 'DELETE_ERROR');
      }
    }

    // Also delete metadata file if exists
    try {
      await this.fs!.unlink(filePath + '.meta.json');
    } catch {
      // Ignore if metadata file doesn't exist
    }
  }

  async list(prefix: string): Promise<string[]> {
    await this.ensureModules();
    const prefixPath = this.resolvePath(prefix);
    const prefixDir = this.path!.dirname(prefixPath);
    const prefixBase = this.path!.basename(prefixPath);

    try {
      const entries = await this.fs!.readdir(prefixDir, { withFileTypes: true });
      const keys: string[] = [];

      for (const entry of entries) {
        if (entry.name.startsWith(prefixBase) && !entry.name.endsWith('.meta.json')) {
          const relativePath = this.path!.join(
            prefix.substring(0, prefix.lastIndexOf('/') + 1),
            entry.name
          );
          keys.push(relativePath);
        }
      }

      return keys.sort();
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') {
        return [];
      }
      throw new StorageError(`Failed to list directory: ${prefix}`, 'LIST_ERROR');
    }
  }

  async exists(key: string): Promise<boolean> {
    await this.ensureModules();
    const filePath = this.resolvePath(key);

    try {
      await this.fs!.access(filePath);
      return true;
    } catch {
      return false;
    }
  }
}
