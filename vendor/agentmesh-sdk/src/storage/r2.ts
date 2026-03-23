import type { Storage, StorageSetOptions } from './interface';
import { StorageError } from '../errors';

/**
 * R2Bucket interface - matches Cloudflare R2 API.
 * Using interface to avoid direct dependency on @cloudflare/workers-types.
 */
interface R2Bucket {
  get(key: string): Promise<R2Object | null>;
  put(key: string, value: ArrayBuffer | Uint8Array, options?: R2PutOptions): Promise<R2Object>;
  delete(key: string | string[]): Promise<void>;
  list(options?: R2ListOptions): Promise<R2ObjectList>;
  head(key: string): Promise<R2Object | null>;
}

interface R2Object {
  arrayBuffer(): Promise<ArrayBuffer>;
  customMetadata?: Record<string, string>;
}

interface R2PutOptions {
  customMetadata?: Record<string, string>;
}

interface R2ListOptions {
  prefix?: string;
  limit?: number;
  cursor?: string;
}

interface R2ObjectList {
  objects: Array<{ key: string }>;
  truncated: boolean;
  cursor?: string;
}

/**
 * Cloudflare R2 storage implementation for MoltWorker environments.
 */
export class R2Storage implements Storage {
  private bucket: R2Bucket;

  constructor(bucket: R2Bucket) {
    this.bucket = bucket;
  }

  async get(key: string): Promise<Uint8Array | null> {
    try {
      const object = await this.bucket.get(key);
      if (!object) {
        return null;
      }

      const buffer = await object.arrayBuffer();
      return new Uint8Array(buffer);
    } catch (error) {
      throw new StorageError(`Failed to get from R2: ${key}`, 'R2_GET_ERROR');
    }
  }

  async set(key: string, value: Uint8Array, options?: StorageSetOptions): Promise<void> {
    try {
      const putOptions: R2PutOptions = {};

      if (options?.metadata) {
        putOptions.customMetadata = options.metadata;
      }

      // Note: R2 doesn't have native TTL support.
      // TTL would need to be implemented via metadata + cleanup job.
      if (options?.ttl !== undefined && options.metadata === undefined) {
        putOptions.customMetadata = {};
      }
      if (options?.ttl !== undefined) {
        putOptions.customMetadata = {
          ...putOptions.customMetadata,
          expiresAt: String(Date.now() + options.ttl * 1000),
        };
      }

      await this.bucket.put(key, value, putOptions);
    } catch (error) {
      throw new StorageError(`Failed to put to R2: ${key}`, 'R2_PUT_ERROR');
    }
  }

  async delete(key: string): Promise<void> {
    try {
      await this.bucket.delete(key);
    } catch (error) {
      throw new StorageError(`Failed to delete from R2: ${key}`, 'R2_DELETE_ERROR');
    }
  }

  async list(prefix: string): Promise<string[]> {
    try {
      const keys: string[] = [];
      let cursor: string | undefined;

      do {
        const result = await this.bucket.list({
          prefix,
          limit: 1000,
          cursor,
        });

        for (const object of result.objects) {
          keys.push(object.key);
        }

        cursor = result.truncated ? result.cursor : undefined;
      } while (cursor);

      return keys.sort();
    } catch (error) {
      throw new StorageError(`Failed to list R2: ${prefix}`, 'R2_LIST_ERROR');
    }
  }

  async exists(key: string): Promise<boolean> {
    try {
      const head = await this.bucket.head(key);
      return head !== null;
    } catch {
      return false;
    }
  }

  /**
   * Get metadata for a key (if any).
   */
  async getMetadata(key: string): Promise<Record<string, string> | null> {
    try {
      const object = await this.bucket.get(key);
      return object?.customMetadata ?? null;
    } catch {
      return null;
    }
  }
}
