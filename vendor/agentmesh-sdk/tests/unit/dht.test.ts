/**
 * Unit tests for DHT module.
 */
import { describe, test, expect, beforeEach } from 'vitest';
import {
  DHTClient,
  KBucket,
  xorDistance,
  compareDistance,
  getBucketIndex,
  createCapabilityKey,
  createAmidKey,
} from '../../src/dht';
import { Identity } from '../../src/identity';

describe('DHT', () => {
  let identity: Identity;

  beforeEach(async () => {
    identity = await Identity.generate();
  });

  describe('KBucket', () => {
    test('should create bucket with default k', () => {
      const bucket = new KBucket();
      expect(bucket.size).toBe(0);
      expect(bucket.isFull).toBe(false);
    });

    test('should add node', () => {
      const bucket = new KBucket(5);
      const added = bucket.addOrUpdate({
        id: 'node-1',
        address: 'http://node1.example.com',
        lastSeen: new Date(),
      });

      expect(added).toBe(true);
      expect(bucket.size).toBe(1);
    });

    test('should update existing node', () => {
      const bucket = new KBucket(5);
      const oldDate = new Date(Date.now() - 10000);

      bucket.addOrUpdate({
        id: 'node-1',
        address: 'http://node1.example.com',
        lastSeen: oldDate,
      });

      bucket.addOrUpdate({
        id: 'node-1',
        address: 'http://node1.example.com',
        lastSeen: new Date(),
      });

      expect(bucket.size).toBe(1);
      const nodes = bucket.getNodes();
      expect(nodes[0]?.lastSeen.getTime()).toBeGreaterThan(oldDate.getTime());
    });

    test('should reject when full', () => {
      const bucket = new KBucket(2);

      bucket.addOrUpdate({ id: 'node-1', address: 'addr1', lastSeen: new Date() });
      bucket.addOrUpdate({ id: 'node-2', address: 'addr2', lastSeen: new Date() });
      const added = bucket.addOrUpdate({ id: 'node-3', address: 'addr3', lastSeen: new Date() });

      expect(added).toBe(false);
      expect(bucket.size).toBe(2);
      expect(bucket.isFull).toBe(true);
    });

    test('should remove node', () => {
      const bucket = new KBucket();
      bucket.addOrUpdate({ id: 'node-1', address: 'addr1', lastSeen: new Date() });
      bucket.addOrUpdate({ id: 'node-2', address: 'addr2', lastSeen: new Date() });

      const removed = bucket.remove('node-1');
      expect(removed).toBe(true);
      expect(bucket.size).toBe(1);
    });

    test('should get oldest node', () => {
      const bucket = new KBucket();
      bucket.addOrUpdate({ id: 'node-1', address: 'addr1', lastSeen: new Date(1000) });
      bucket.addOrUpdate({ id: 'node-2', address: 'addr2', lastSeen: new Date(2000) });

      const oldest = bucket.getOldest();
      expect(oldest?.id).toBe('node-1');
    });

    test('should clear bucket', () => {
      const bucket = new KBucket();
      bucket.addOrUpdate({ id: 'node-1', address: 'addr1', lastSeen: new Date() });
      bucket.addOrUpdate({ id: 'node-2', address: 'addr2', lastSeen: new Date() });

      bucket.clear();
      expect(bucket.size).toBe(0);
    });
  });

  describe('XOR Distance', () => {
    test('should calculate XOR distance', () => {
      const dist = xorDistance('0000', '0001');
      expect(dist).toBeDefined();
      expect(dist.length).toBeGreaterThan(0);
    });

    test('should have zero distance for same ID', () => {
      const dist = xorDistance('abcd', 'abcd');
      const allZeros = dist.every(b => b === 0);
      expect(allZeros).toBe(true);
    });

    test('should compare distances correctly', () => {
      const dist1 = new Uint8Array([0, 1]);
      const dist2 = new Uint8Array([0, 2]);

      expect(compareDistance(dist1, dist2)).toBe(-1);
      expect(compareDistance(dist2, dist1)).toBe(1);
      expect(compareDistance(dist1, dist1)).toBe(0);
    });

    test('should get bucket index', () => {
      const index = getBucketIndex('00000000', '00000001');
      expect(typeof index).toBe('number');
    });
  });

  describe('DHTClient', () => {
    test('should create client', () => {
      const client = new DHTClient(identity);
      expect(client).toBeDefined();
      expect(client.isAvailable).toBe(false);
    });

    test('should connect and disconnect', async () => {
      const client = new DHTClient(identity, {
        bootstrapNodes: [],
      });

      await client.connect();
      // No bootstrap nodes, but should still connect
      expect(client.isAvailable).toBe(false); // No nodes yet

      await client.disconnect();
    });

    test('should add node', async () => {
      const client = new DHTClient(identity);
      await client.connect();

      const added = client.addNode({
        id: 'different-node-id',
        address: 'http://node.example.com',
        lastSeen: new Date(),
      });

      expect(added).toBe(true);
      expect(client.getKnownNodesCount()).toBe(1);
    });

    test('should not add self', async () => {
      const client = new DHTClient(identity);
      await client.connect();

      const added = client.addNode({
        id: identity.amid,
        address: 'http://self.example.com',
        lastSeen: new Date(),
      });

      expect(added).toBe(false);
    });

    test('should put and get value', async () => {
      const client = new DHTClient(identity);
      await client.connect();

      const key = 'test-key';
      const value = new TextEncoder().encode('test-value');

      await client.put(key, value, 3600);

      const retrieved = await client.get(key);
      expect(retrieved).toBeDefined();
      expect(new TextDecoder().decode(retrieved?.value)).toBe('test-value');
    });

    test('should return null for missing key', async () => {
      const client = new DHTClient(identity);
      await client.connect();

      const result = await client.get('nonexistent-key');
      expect(result).toBeNull();
    });

    test('should register agent capabilities', async () => {
      const client = new DHTClient(identity);
      await client.connect();

      await client.registerAgent(['weather/forecast', 'travel/flights']);

      const metrics = client.getMetrics();
      expect(metrics.storedEntries).toBeGreaterThan(0);
    });

    test('should find agents by capability', async () => {
      const client = new DHTClient(identity);
      await client.connect();

      await client.registerAgent(['weather/forecast']);
      const agents = await client.findAgents('weather/forecast');

      expect(agents.length).toBe(1);
      expect(agents[0]?.amid).toBe(identity.amid);
    });

    test('should get closest nodes', async () => {
      const client = new DHTClient(identity);
      await client.connect();

      // Add some nodes
      for (let i = 0; i < 5; i++) {
        client.addNode({
          id: `node-${i}-different`,
          address: `http://node${i}.example.com`,
          lastSeen: new Date(),
        });
      }

      const closest = client.getClosestNodes('target-id', 3);
      expect(closest.length).toBeLessThanOrEqual(3);
    });

    test('should get metrics', async () => {
      const client = new DHTClient(identity);
      await client.connect();

      const metrics = client.getMetrics();
      expect(metrics.knownNodes).toBe(0);
      expect(metrics.storedEntries).toBe(0);
      expect(metrics.lookups).toBe(0);
    });

    test('should clear data', async () => {
      const client = new DHTClient(identity);
      await client.connect();

      client.addNode({ id: 'test-node', address: 'addr', lastSeen: new Date() });
      await client.put('key', new TextEncoder().encode('value'));

      client.clear();

      expect(client.getKnownNodesCount()).toBe(0);
      expect(client.getMetrics().storedEntries).toBe(0);
    });
  });

  describe('Key Helpers', () => {
    test('should create capability key', () => {
      const key = createCapabilityKey('weather/forecast');
      expect(key).toBe('cap:weather/forecast');
    });

    test('should create AMID key', () => {
      const key = createAmidKey('test-amid-12345');
      expect(key).toBe('agent:test-amid-12345');
    });
  });
});
