import { describe, it, expect, beforeEach } from 'vitest';
import { MemoryStorage, StorageNamespace, namespacedKey, stripNamespace } from '../../src/storage/index';

describe('MemoryStorage', () => {
  let storage: MemoryStorage;

  beforeEach(() => {
    storage = new MemoryStorage();
  });

  describe('get/set', () => {
    it('should store and retrieve values', async () => {
      const key = 'test-key';
      const value = new Uint8Array([1, 2, 3, 4]);

      await storage.set(key, value);
      const retrieved = await storage.get(key);

      expect(retrieved).toEqual(value);
    });

    it('should return null for non-existent keys', async () => {
      const result = await storage.get('non-existent');
      expect(result).toBeNull();
    });

    it('should overwrite existing values', async () => {
      const key = 'test-key';
      await storage.set(key, new Uint8Array([1, 2]));
      await storage.set(key, new Uint8Array([3, 4]));

      const result = await storage.get(key);
      expect(result).toEqual(new Uint8Array([3, 4]));
    });

    it('should copy values to prevent mutation', async () => {
      const key = 'test-key';
      const value = new Uint8Array([1, 2, 3]);

      await storage.set(key, value);
      value[0] = 99; // Mutate original

      const retrieved = await storage.get(key);
      expect(retrieved![0]).toBe(1); // Should still be 1
    });
  });

  describe('TTL', () => {
    it('should expire values after TTL', async () => {
      const key = 'ttl-key';
      const value = new Uint8Array([1, 2, 3]);

      // Set with very short TTL
      await storage.set(key, value, { ttl: 0.001 }); // 1ms

      // Wait for expiration
      await new Promise((resolve) => setTimeout(resolve, 10));

      const result = await storage.get(key);
      expect(result).toBeNull();
    });

    it('should keep values before TTL expires', async () => {
      const key = 'ttl-key';
      const value = new Uint8Array([1, 2, 3]);

      await storage.set(key, value, { ttl: 60 }); // 60 seconds

      const result = await storage.get(key);
      expect(result).toEqual(value);
    });
  });

  describe('delete', () => {
    it('should delete existing values', async () => {
      const key = 'delete-key';
      await storage.set(key, new Uint8Array([1, 2, 3]));

      await storage.delete(key);

      const result = await storage.get(key);
      expect(result).toBeNull();
    });

    it('should not throw when deleting non-existent keys', async () => {
      await expect(storage.delete('non-existent')).resolves.not.toThrow();
    });
  });

  describe('list', () => {
    it('should list keys with prefix', async () => {
      await storage.set('prefix/a', new Uint8Array([1]));
      await storage.set('prefix/b', new Uint8Array([2]));
      await storage.set('other/c', new Uint8Array([3]));

      const keys = await storage.list('prefix/');

      expect(keys).toEqual(['prefix/a', 'prefix/b']);
    });

    it('should return empty array for no matches', async () => {
      await storage.set('other/a', new Uint8Array([1]));

      const keys = await storage.list('prefix/');

      expect(keys).toEqual([]);
    });

    it('should return sorted keys', async () => {
      await storage.set('prefix/c', new Uint8Array([1]));
      await storage.set('prefix/a', new Uint8Array([2]));
      await storage.set('prefix/b', new Uint8Array([3]));

      const keys = await storage.list('prefix/');

      expect(keys).toEqual(['prefix/a', 'prefix/b', 'prefix/c']);
    });

    it('should exclude expired keys', async () => {
      await storage.set('prefix/expired', new Uint8Array([1]), { ttl: 0.001 });
      await storage.set('prefix/valid', new Uint8Array([2]));

      await new Promise((resolve) => setTimeout(resolve, 10));

      const keys = await storage.list('prefix/');
      expect(keys).toEqual(['prefix/valid']);
    });
  });

  describe('exists', () => {
    it('should return true for existing keys', async () => {
      await storage.set('exists-key', new Uint8Array([1]));

      const result = await storage.exists('exists-key');
      expect(result).toBe(true);
    });

    it('should return false for non-existent keys', async () => {
      const result = await storage.exists('non-existent');
      expect(result).toBe(false);
    });

    it('should return false for expired keys', async () => {
      await storage.set('expired-key', new Uint8Array([1]), { ttl: 0.001 });

      await new Promise((resolve) => setTimeout(resolve, 10));

      const result = await storage.exists('expired-key');
      expect(result).toBe(false);
    });
  });

  describe('clear', () => {
    it('should remove all values', async () => {
      await storage.set('a', new Uint8Array([1]));
      await storage.set('b', new Uint8Array([2]));

      storage.clear();

      expect(storage.size).toBe(0);
      expect(await storage.get('a')).toBeNull();
      expect(await storage.get('b')).toBeNull();
    });
  });

  describe('metadata', () => {
    it('should store and retrieve metadata', async () => {
      const key = 'meta-key';
      const metadata = { foo: 'bar', baz: 'qux' };

      await storage.set(key, new Uint8Array([1]), { metadata });

      const result = await storage.getMetadata(key);
      expect(result).toEqual(metadata);
    });

    it('should return null for keys without metadata', async () => {
      await storage.set('no-meta', new Uint8Array([1]));

      const result = await storage.getMetadata('no-meta');
      expect(result).toBeNull();
    });
  });
});

describe('Storage Namespacing', () => {
  describe('namespacedKey', () => {
    it('should prefix key with namespace', () => {
      const result = namespacedKey(StorageNamespace.IDENTITY, 'my-identity');
      expect(result).toBe('identity/my-identity');
    });

    it('should work with all namespaces', () => {
      expect(namespacedKey(StorageNamespace.SESSIONS, 'abc123')).toBe('sessions/abc123');
      expect(namespacedKey(StorageNamespace.PREKEYS, 'key1')).toBe('prekeys/key1');
      expect(namespacedKey(StorageNamespace.AUDIT, 'event1')).toBe('audit/event1');
      expect(namespacedKey(StorageNamespace.CACHE, 'data1')).toBe('cache/data1');
    });
  });

  describe('stripNamespace', () => {
    it('should remove namespace prefix', () => {
      const result = stripNamespace(StorageNamespace.IDENTITY, 'identity/my-identity');
      expect(result).toBe('my-identity');
    });

    it('should return original if namespace not present', () => {
      const result = stripNamespace(StorageNamespace.IDENTITY, 'other/my-identity');
      expect(result).toBe('other/my-identity');
    });
  });
});
