'use strict';

var chunkFNHOFD2H_cjs = require('./chunk-FNHOFD2H.cjs');
var os = require('os');
var nodePath = require('path');

function _interopNamespace(e) {
  if (e && e.__esModule) return e;
  var n = Object.create(null);
  if (e) {
    Object.keys(e).forEach(function (k) {
      if (k !== 'default') {
        var d = Object.getOwnPropertyDescriptor(e, k);
        Object.defineProperty(n, k, d.get ? d : {
          enumerable: true,
          get: function () { return e[k]; }
        });
      }
    });
  }
  n.default = e;
  return Object.freeze(n);
}

var os__namespace = /*#__PURE__*/_interopNamespace(os);
var nodePath__namespace = /*#__PURE__*/_interopNamespace(nodePath);

// src/storage/memory.ts
var MemoryStorage = class {
  store = /* @__PURE__ */ new Map();
  async get(key) {
    const entry = this.store.get(key);
    if (!entry) {
      return null;
    }
    if (entry.expiresAt !== void 0 && Date.now() > entry.expiresAt) {
      this.store.delete(key);
      return null;
    }
    return entry.data;
  }
  async set(key, value, options) {
    const entry = {
      data: new Uint8Array(value),
      // Copy to prevent external mutation
      metadata: options?.metadata
    };
    if (options?.ttl !== void 0) {
      entry.expiresAt = Date.now() + options.ttl * 1e3;
    }
    this.store.set(key, entry);
  }
  async delete(key) {
    this.store.delete(key);
  }
  async list(prefix) {
    const keys = [];
    const now = Date.now();
    for (const [key, entry] of this.store) {
      if (entry.expiresAt !== void 0 && now > entry.expiresAt) {
        this.store.delete(key);
        continue;
      }
      if (key.startsWith(prefix)) {
        keys.push(key);
      }
    }
    return keys.sort();
  }
  async exists(key) {
    const entry = this.store.get(key);
    if (!entry) {
      return false;
    }
    if (entry.expiresAt !== void 0 && Date.now() > entry.expiresAt) {
      this.store.delete(key);
      return false;
    }
    return true;
  }
  /**
   * Clear all stored values (useful for testing).
   */
  clear() {
    this.store.clear();
  }
  /**
   * Get the number of stored values.
   */
  get size() {
    return this.store.size;
  }
  /**
   * Get metadata for a key (if any).
   */
  async getMetadata(key) {
    const entry = this.store.get(key);
    return entry?.metadata ?? null;
  }
};
function getDefaultAgentMeshPath() {
  if (process.env.AGENTMESH_HOME) {
    return process.env.AGENTMESH_HOME;
  }
  return nodePath__namespace.join(os__namespace.homedir(), ".agentmesh");
}
function createDefaultFileStorage() {
  return new FileStorage(getDefaultAgentMeshPath());
}
var FileStorage = class {
  basePath;
  fs = null;
  path = null;
  constructor(basePath) {
    this.basePath = basePath;
  }
  async ensureModules() {
    if (this.fs === null || this.path === null) {
      try {
        this.fs = await import('fs/promises');
        this.path = await import('path');
      } catch {
        throw new chunkFNHOFD2H_cjs.StorageError(
          "FileStorage requires Node.js environment with fs module",
          "FS_NOT_AVAILABLE"
        );
      }
    }
  }
  async ensureDir(filePath) {
    await this.ensureModules();
    const dir = this.path.dirname(filePath);
    await this.fs.mkdir(dir, { recursive: true });
  }
  resolvePath(key) {
    const sanitized = key.replace(/\.\./g, "_").replace(/^\//, "");
    return this.path.join(this.basePath, sanitized);
  }
  async get(key) {
    await this.ensureModules();
    const filePath = this.resolvePath(key);
    try {
      const data = await this.fs.readFile(filePath);
      return new Uint8Array(data);
    } catch {
      if (error.code === "ENOENT") {
        return null;
      }
      throw new chunkFNHOFD2H_cjs.StorageError(`Failed to read file: ${key}`, "READ_ERROR");
    }
  }
  async set(key, value, options) {
    await this.ensureModules();
    const filePath = this.resolvePath(key);
    try {
      await this.ensureDir(filePath);
      await this.fs.writeFile(filePath, value, { mode: 384 });
      if (options?.metadata) {
        const metaPath = filePath + ".meta.json";
        const metaData = {
          ...options.metadata,
          ...options.ttl !== void 0 && { expiresAt: Date.now() + options.ttl * 1e3 }
        };
        await this.fs.writeFile(metaPath, JSON.stringify(metaData), { mode: 384 });
      }
    } catch {
      throw new chunkFNHOFD2H_cjs.StorageError(`Failed to write file: ${key}`, "WRITE_ERROR");
    }
  }
  async delete(key) {
    await this.ensureModules();
    const filePath = this.resolvePath(key);
    try {
      await this.fs.unlink(filePath);
    } catch {
      if (error.code !== "ENOENT") {
        throw new chunkFNHOFD2H_cjs.StorageError(`Failed to delete file: ${key}`, "DELETE_ERROR");
      }
    }
    try {
      await this.fs.unlink(filePath + ".meta.json");
    } catch {
    }
  }
  async list(prefix) {
    await this.ensureModules();
    const prefixPath = this.resolvePath(prefix);
    const prefixDir = this.path.dirname(prefixPath);
    const prefixBase = this.path.basename(prefixPath);
    try {
      const entries = await this.fs.readdir(prefixDir, { withFileTypes: true });
      const keys = [];
      for (const entry of entries) {
        if (entry.name.startsWith(prefixBase) && !entry.name.endsWith(".meta.json")) {
          const relativePath = this.path.join(
            prefix.substring(0, prefix.lastIndexOf("/") + 1),
            entry.name
          );
          keys.push(relativePath);
        }
      }
      return keys.sort();
    } catch {
      if (error.code === "ENOENT") {
        return [];
      }
      throw new chunkFNHOFD2H_cjs.StorageError(`Failed to list directory: ${prefix}`, "LIST_ERROR");
    }
  }
  async exists(key) {
    await this.ensureModules();
    const filePath = this.resolvePath(key);
    try {
      await this.fs.access(filePath);
      return true;
    } catch {
      return false;
    }
  }
};

// src/storage/r2.ts
var R2Storage = class {
  bucket;
  constructor(bucket) {
    this.bucket = bucket;
  }
  async get(key) {
    try {
      const object = await this.bucket.get(key);
      if (!object) {
        return null;
      }
      const buffer = await object.arrayBuffer();
      return new Uint8Array(buffer);
    } catch {
      throw new chunkFNHOFD2H_cjs.StorageError(`Failed to get from R2: ${key}`, "R2_GET_ERROR");
    }
  }
  async set(key, value, options) {
    try {
      const putOptions = {};
      if (options?.metadata) {
        putOptions.customMetadata = options.metadata;
      }
      if (options?.ttl !== void 0 && options.metadata === void 0) {
        putOptions.customMetadata = {};
      }
      if (options?.ttl !== void 0) {
        putOptions.customMetadata = {
          ...putOptions.customMetadata,
          expiresAt: String(Date.now() + options.ttl * 1e3)
        };
      }
      await this.bucket.put(key, value, putOptions);
    } catch {
      throw new chunkFNHOFD2H_cjs.StorageError(`Failed to put to R2: ${key}`, "R2_PUT_ERROR");
    }
  }
  async delete(key) {
    try {
      await this.bucket.delete(key);
    } catch {
      throw new chunkFNHOFD2H_cjs.StorageError(`Failed to delete from R2: ${key}`, "R2_DELETE_ERROR");
    }
  }
  async list(prefix) {
    try {
      const keys = [];
      let cursor;
      do {
        const result = await this.bucket.list({
          prefix,
          limit: 1e3,
          cursor
        });
        for (const object of result.objects) {
          keys.push(object.key);
        }
        cursor = result.truncated ? result.cursor : void 0;
      } while (cursor);
      return keys.sort();
    } catch {
      throw new chunkFNHOFD2H_cjs.StorageError(`Failed to list R2: ${prefix}`, "R2_LIST_ERROR");
    }
  }
  async exists(key) {
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
  async getMetadata(key) {
    try {
      const object = await this.bucket.get(key);
      return object?.customMetadata ?? null;
    } catch {
      return null;
    }
  }
};

// src/storage/kv.ts
var KVStorage = class {
  namespace;
  constructor(namespace) {
    this.namespace = namespace;
  }
  async get(key) {
    try {
      const buffer = await this.namespace.get(key, { type: "arrayBuffer" });
      if (!buffer) {
        return null;
      }
      return new Uint8Array(buffer);
    } catch {
      throw new chunkFNHOFD2H_cjs.StorageError(`Failed to get from KV: ${key}`, "KV_GET_ERROR");
    }
  }
  async set(key, value, options) {
    try {
      const putOptions = {};
      if (options?.ttl !== void 0) {
        putOptions.expirationTtl = options.ttl;
      }
      if (options?.metadata) {
        putOptions.metadata = options.metadata;
      }
      await this.namespace.put(key, value, putOptions);
    } catch {
      throw new chunkFNHOFD2H_cjs.StorageError(`Failed to put to KV: ${key}`, "KV_PUT_ERROR");
    }
  }
  async delete(key) {
    try {
      await this.namespace.delete(key);
    } catch {
      throw new chunkFNHOFD2H_cjs.StorageError(`Failed to delete from KV: ${key}`, "KV_DELETE_ERROR");
    }
  }
  async list(prefix) {
    try {
      const keys = [];
      let cursor;
      do {
        const result = await this.namespace.list({
          prefix,
          limit: 1e3,
          cursor
        });
        for (const key of result.keys) {
          keys.push(key.name);
        }
        cursor = result.list_complete ? void 0 : result.cursor;
      } while (cursor);
      return keys.sort();
    } catch {
      throw new chunkFNHOFD2H_cjs.StorageError(`Failed to list KV: ${prefix}`, "KV_LIST_ERROR");
    }
  }
  async exists(key) {
    try {
      const value = await this.namespace.get(key, { type: "arrayBuffer" });
      return value !== null;
    } catch {
      return false;
    }
  }
  /**
   * Get value with metadata.
   */
  async getWithMetadata(key) {
    try {
      const result = await this.namespace.getWithMetadata(key, { type: "arrayBuffer" });
      return {
        value: result.value ? new Uint8Array(result.value) : null,
        metadata: result.metadata
      };
    } catch {
      throw new chunkFNHOFD2H_cjs.StorageError(`Failed to get with metadata from KV: ${key}`, "KV_GET_ERROR");
    }
  }
};

exports.FileStorage = FileStorage;
exports.KVStorage = KVStorage;
exports.MemoryStorage = MemoryStorage;
exports.R2Storage = R2Storage;
exports.createDefaultFileStorage = createDefaultFileStorage;
exports.getDefaultAgentMeshPath = getDefaultAgentMeshPath;
//# sourceMappingURL=chunk-S7F5N5HC.cjs.map
//# sourceMappingURL=chunk-S7F5N5HC.cjs.map