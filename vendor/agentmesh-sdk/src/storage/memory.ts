import type { Storage, StorageSetOptions } from './interface';

interface StoredValue {
  data: Uint8Array;
  expiresAt?: number;
  metadata?: Record<string, string>;
}

/**
 * In-memory storage implementation for testing and development.
 */
export class MemoryStorage implements Storage {
  private store: Map<string, StoredValue> = new Map();

  async get(key: string): Promise<Uint8Array | null> {
    const entry = this.store.get(key);
    if (!entry) {
      return null;
    }

    // Check TTL expiration
    if (entry.expiresAt !== undefined && Date.now() > entry.expiresAt) {
      this.store.delete(key);
      return null;
    }

    return entry.data;
  }

  async set(key: string, value: Uint8Array, options?: StorageSetOptions): Promise<void> {
    const entry: StoredValue = {
      data: new Uint8Array(value), // Copy to prevent external mutation
      metadata: options?.metadata,
    };

    if (options?.ttl !== undefined) {
      entry.expiresAt = Date.now() + options.ttl * 1000;
    }

    this.store.set(key, entry);
  }

  async delete(key: string): Promise<void> {
    this.store.delete(key);
  }

  async list(prefix: string): Promise<string[]> {
    const keys: string[] = [];
    const now = Date.now();

    for (const [key, entry] of this.store) {
      // Skip expired entries
      if (entry.expiresAt !== undefined && now > entry.expiresAt) {
        this.store.delete(key);
        continue;
      }

      if (key.startsWith(prefix)) {
        keys.push(key);
      }
    }

    return keys.sort();
  }

  async exists(key: string): Promise<boolean> {
    const entry = this.store.get(key);
    if (!entry) {
      return false;
    }

    // Check TTL expiration
    if (entry.expiresAt !== undefined && Date.now() > entry.expiresAt) {
      this.store.delete(key);
      return false;
    }

    return true;
  }

  /**
   * Clear all stored values (useful for testing).
   */
  clear(): void {
    this.store.clear();
  }

  /**
   * Get the number of stored values.
   */
  get size(): number {
    return this.store.size;
  }

  /**
   * Get metadata for a key (if any).
   */
  async getMetadata(key: string): Promise<Record<string, string> | null> {
    const entry = this.store.get(key);
    return entry?.metadata ?? null;
  }
}
