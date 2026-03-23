/**
 * Storage module - provides pluggable storage backends.
 *
 * @example
 * ```typescript
 * import { MemoryStorage, FileStorage, R2Storage } from '@agentmesh/sdk/storage';
 *
 * // For testing
 * const memory = new MemoryStorage();
 *
 * // For Node.js
 * const file = new FileStorage('/path/to/data');
 *
 * // For MoltWorker
 * const r2 = new R2Storage(env.MY_BUCKET);
 * ```
 */

export type { Storage, StorageSetOptions } from './interface';
export { StorageNamespace, namespacedKey, stripNamespace } from './interface';
export { MemoryStorage } from './memory';
export { FileStorage, getDefaultAgentMeshPath, createDefaultFileStorage } from './file';
export { R2Storage } from './r2';
export { KVStorage } from './kv';
