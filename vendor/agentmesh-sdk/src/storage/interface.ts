/**
 * Abstract storage interface for AgentMesh SDK.
 * Supports multiple backends: R2, filesystem, KV, memory.
 */
export interface Storage {
  /**
   * Get a value by key.
   * @param key - The storage key
   * @returns The value as Uint8Array, or null if not found
   */
  get(key: string): Promise<Uint8Array | null>;

  /**
   * Set a value by key.
   * @param key - The storage key
   * @param value - The value to store
   * @param options - Optional storage options (TTL, etc.)
   */
  set(key: string, value: Uint8Array, options?: StorageSetOptions): Promise<void>;

  /**
   * Delete a value by key.
   * @param key - The storage key
   */
  delete(key: string): Promise<void>;

  /**
   * List keys with a given prefix.
   * @param prefix - The key prefix to filter by
   * @returns Array of matching keys
   */
  list(prefix: string): Promise<string[]>;

  /**
   * Check if a key exists.
   * @param key - The storage key
   * @returns True if the key exists
   */
  exists(key: string): Promise<boolean>;
}

/**
 * Options for storage set operations.
 */
export interface StorageSetOptions {
  /**
   * Time-to-live in seconds. After this time, the value may be deleted.
   */
  ttl?: number;

  /**
   * Optional metadata to store with the value.
   */
  metadata?: Record<string, string>;
}

/**
 * Storage key namespace prefixes.
 */
export const StorageNamespace = {
  IDENTITY: 'identity/',
  SESSIONS: 'sessions/',
  PREKEYS: 'prekeys/',
  AUDIT: 'audit/',
  CACHE: 'cache/',
} as const;

/**
 * Utility to create namespaced storage keys.
 */
export function namespacedKey(namespace: string, key: string): string {
  return `${namespace}${key}`;
}

/**
 * Utility to strip namespace from a key.
 */
export function stripNamespace(namespace: string, key: string): string {
  if (key.startsWith(namespace)) {
    return key.slice(namespace.length);
  }
  return key;
}
