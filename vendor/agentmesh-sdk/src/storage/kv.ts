import type { Storage, StorageSetOptions } from './interface';
import { StorageError } from '../errors';

/**
 * KVNamespace interface - matches Cloudflare Workers KV API.
 */
interface KVNamespace {
  get(key: string, options: { type: 'arrayBuffer' }): Promise<ArrayBuffer | null>;
  get(key: string, options?: { type?: 'text' }): Promise<string | null>;
  put(key: string, value: string | ArrayBuffer | Uint8Array, options?: KVPutOptions): Promise<void>;
  delete(key: string): Promise<void>;
  list(options?: KVListOptions): Promise<KVListResult>;
  getWithMetadata<T = unknown>(
    key: string,
    options: { type: 'arrayBuffer' }
  ): Promise<{ value: ArrayBuffer | null; metadata: T | null }>;
}

interface KVPutOptions {
  expirationTtl?: number;
  expiration?: number;
  metadata?: Record<string, unknown>;
}

interface KVListOptions {
  prefix?: string;
  limit?: number;
  cursor?: string;
}

interface KVListResult {
  keys: Array<{ name: string; expiration?: number; metadata?: unknown }>;
  list_complete: boolean;
  cursor?: string;
}

/**
 * Cloudflare Workers KV storage implementation.
 */
export class KVStorage implements Storage {
  private namespace: KVNamespace;

  constructor(namespace: KVNamespace) {
    this.namespace = namespace;
  }

  async get(key: string): Promise<Uint8Array | null> {
    try {
      const buffer = await this.namespace.get(key, { type: 'arrayBuffer' });
      if (!buffer) {
        return null;
      }
      return new Uint8Array(buffer);
    } catch (error) {
      throw new StorageError(`Failed to get from KV: ${key}`, 'KV_GET_ERROR');
    }
  }

  async set(key: string, value: Uint8Array, options?: StorageSetOptions): Promise<void> {
    try {
      const putOptions: KVPutOptions = {};

      if (options?.ttl !== undefined) {
        putOptions.expirationTtl = options.ttl;
      }

      if (options?.metadata) {
        putOptions.metadata = options.metadata;
      }

      await this.namespace.put(key, value, putOptions);
    } catch (error) {
      throw new StorageError(`Failed to put to KV: ${key}`, 'KV_PUT_ERROR');
    }
  }

  async delete(key: string): Promise<void> {
    try {
      await this.namespace.delete(key);
    } catch (error) {
      throw new StorageError(`Failed to delete from KV: ${key}`, 'KV_DELETE_ERROR');
    }
  }

  async list(prefix: string): Promise<string[]> {
    try {
      const keys: string[] = [];
      let cursor: string | undefined;

      do {
        const result = await this.namespace.list({
          prefix,
          limit: 1000,
          cursor,
        });

        for (const key of result.keys) {
          keys.push(key.name);
        }

        cursor = result.list_complete ? undefined : result.cursor;
      } while (cursor);

      return keys.sort();
    } catch (error) {
      throw new StorageError(`Failed to list KV: ${prefix}`, 'KV_LIST_ERROR');
    }
  }

  async exists(key: string): Promise<boolean> {
    try {
      const value = await this.namespace.get(key, { type: 'arrayBuffer' });
      return value !== null;
    } catch {
      return false;
    }
  }

  /**
   * Get value with metadata.
   */
  async getWithMetadata(key: string): Promise<{
    value: Uint8Array | null;
    metadata: Record<string, unknown> | null;
  }> {
    try {
      const result = await this.namespace.getWithMetadata(key, { type: 'arrayBuffer' });
      return {
        value: result.value ? new Uint8Array(result.value) : null,
        metadata: result.metadata as Record<string, unknown> | null,
      };
    } catch (error) {
      throw new StorageError(`Failed to get with metadata from KV: ${key}`, 'KV_GET_ERROR');
    }
  }
}
