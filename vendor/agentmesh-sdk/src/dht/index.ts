/**
 * DHT (Distributed Hash Table) module for AgentMesh.
 * Implements a Kademlia-like DHT for peer discovery.
 */

import { Identity } from '../identity';
import { NetworkError, ValidationError } from '../errors';

/**
 * DHT node information.
 */
export interface DHTNode {
  /** Node ID (AMID) */
  id: string;
  /** Node address */
  address: string;
  /** Last seen timestamp */
  lastSeen: Date;
  /** Node's public key */
  publicKey?: Uint8Array;
}

/**
 * DHT key-value entry.
 */
export interface DHTEntry {
  /** Key */
  key: string;
  /** Value */
  value: Uint8Array;
  /** Publisher's AMID */
  publisher: string;
  /** Signature of the value */
  signature: Uint8Array;
  /** Expiration timestamp */
  expiresAt: Date;
  /** Last updated timestamp */
  updatedAt: Date;
}

/**
 * DHT capability registration.
 */
export interface DHTCapabilityEntry {
  /** Agent's AMID */
  amid: string;
  /** Capabilities offered */
  capabilities: string[];
  /** Agent's address (relay URL or direct) */
  address: string;
  /** Timestamp */
  timestamp: number;
  /** Signature */
  signature: string;
}

/**
 * DHT metrics.
 */
export interface DHTMetrics {
  /** Number of nodes in routing table */
  knownNodes: number;
  /** Number of stored entries */
  storedEntries: number;
  /** Number of lookups performed */
  lookups: number;
  /** Average lookup time in ms */
  avgLookupTimeMs: number;
  /** Number of failed lookups */
  failedLookups: number;
}

/**
 * K-bucket for Kademlia routing table.
 */
export class KBucket {
  private readonly k: number;
  private nodes: DHTNode[] = [];

  constructor(k: number = 20) {
    this.k = k;
  }

  /**
   * Get all nodes in this bucket.
   */
  getNodes(): DHTNode[] {
    return [...this.nodes];
  }

  /**
   * Get the number of nodes in this bucket.
   */
  get size(): number {
    return this.nodes.length;
  }

  /**
   * Check if the bucket is full.
   */
  get isFull(): boolean {
    return this.nodes.length >= this.k;
  }

  /**
   * Add or update a node in the bucket.
   */
  addOrUpdate(node: DHTNode): boolean {
    // Check if node already exists
    const existingIndex = this.nodes.findIndex(n => n.id === node.id);

    if (existingIndex !== -1) {
      // Move to end (most recently seen)
      this.nodes.splice(existingIndex, 1);
      this.nodes.push({ ...node, lastSeen: new Date() });
      return true;
    }

    if (!this.isFull) {
      this.nodes.push(node);
      return true;
    }

    // Bucket is full - could implement ping-based replacement
    // For now, reject the node
    return false;
  }

  /**
   * Remove a node from the bucket.
   */
  remove(nodeId: string): boolean {
    const index = this.nodes.findIndex(n => n.id === nodeId);
    if (index !== -1) {
      this.nodes.splice(index, 1);
      return true;
    }
    return false;
  }

  /**
   * Get the oldest node (for potential eviction).
   */
  getOldest(): DHTNode | undefined {
    return this.nodes[0];
  }

  /**
   * Clear all nodes from the bucket.
   */
  clear(): void {
    this.nodes = [];
  }
}

/**
 * Calculate XOR distance between two node IDs.
 */
export function xorDistance(id1: string, id2: string): Uint8Array {
  // Decode IDs as bytes
  const bytes1 = decodeId(id1);
  const bytes2 = decodeId(id2);

  // Ensure same length
  const length = Math.max(bytes1.length, bytes2.length);
  const result = new Uint8Array(length);

  for (let i = 0; i < length; i++) {
    const b1 = i < bytes1.length ? bytes1[i]! : 0;
    const b2 = i < bytes2.length ? bytes2[i]! : 0;
    result[i] = b1 ^ b2;
  }

  return result;
}

/**
 * Compare two XOR distances.
 * Returns -1 if a < b, 0 if a == b, 1 if a > b.
 */
export function compareDistance(a: Uint8Array, b: Uint8Array): number {
  const length = Math.max(a.length, b.length);

  for (let i = 0; i < length; i++) {
    const ai = i < a.length ? a[i]! : 0;
    const bi = i < b.length ? b[i]! : 0;

    if (ai < bi) return -1;
    if (ai > bi) return 1;
  }

  return 0;
}

/**
 * Get the bucket index for a node ID relative to our ID.
 */
export function getBucketIndex(ourId: string, nodeId: string): number {
  const distance = xorDistance(ourId, nodeId);

  // Find the first non-zero bit
  for (let i = 0; i < distance.length; i++) {
    const byte = distance[i]!;
    if (byte !== 0) {
      // Find the leading bit
      for (let bit = 7; bit >= 0; bit--) {
        if ((byte & (1 << bit)) !== 0) {
          return i * 8 + (7 - bit);
        }
      }
    }
  }

  // Same ID
  return 0;
}

/**
 * Decode a node ID to bytes.
 */
function decodeId(id: string): Uint8Array {
  // Base58 decode (simplified)
  const ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';

  let num = 0n;
  for (const char of id) {
    const index = ALPHABET.indexOf(char);
    if (index === -1) {
      // Fall back to hex encoding
      const bytes = new Uint8Array(id.length / 2);
      for (let i = 0; i < id.length; i += 2) {
        bytes[i / 2] = parseInt(id.slice(i, i + 2), 16);
      }
      return bytes;
    }
    num = num * 58n + BigInt(index);
  }

  // Convert to bytes
  const bytes: number[] = [];
  while (num > 0n) {
    bytes.unshift(Number(num % 256n));
    num = num / 256n;
  }

  // Handle leading '1's
  for (const char of id) {
    if (char === '1') {
      bytes.unshift(0);
    } else {
      break;
    }
  }

  return new Uint8Array(bytes);
}

/**
 * DHT Client for interacting with the distributed hash table.
 */
export class DHTClient {
  private identity: Identity;
  private bootstrapNodes: string[];
  private buckets: Map<number, KBucket> = new Map();
  private storage: Map<string, DHTEntry> = new Map();
  private metrics: DHTMetrics;
  private k: number;
  private alpha: number;
  private connected: boolean = false;

  constructor(
    identity: Identity,
    options?: {
      bootstrapNodes?: string[];
      k?: number;
      alpha?: number;
    }
  ) {
    this.identity = identity;
    this.bootstrapNodes = options?.bootstrapNodes || [];
    this.k = options?.k || 20;
    this.alpha = options?.alpha || 3;

    this.metrics = {
      knownNodes: 0,
      storedEntries: 0,
      lookups: 0,
      avgLookupTimeMs: 0,
      failedLookups: 0,
    };
  }

  /**
   * Connect to the DHT network.
   */
  async connect(): Promise<void> {
    if (this.connected) return;

    // Bootstrap by connecting to known nodes
    for (const nodeUrl of this.bootstrapNodes) {
      try {
        await this.pingNode(nodeUrl);
      } catch {
        // Ignore failed bootstrap nodes
      }
    }

    this.connected = true;
  }

  /**
   * Disconnect from the DHT network.
   */
  async disconnect(): Promise<void> {
    this.connected = false;
    this.buckets.clear();
  }

  /**
   * Check if connected to the DHT.
   */
  get isAvailable(): boolean {
    return this.connected && this.getKnownNodesCount() > 0;
  }

  /**
   * Ping a node to check if it's alive.
   */
  async pingNode(nodeUrl: string): Promise<DHTNode | null> {
    // In a real implementation, this would make a network request
    // For now, simulate a ping
    const node: DHTNode = {
      id: this.hashUrl(nodeUrl),
      address: nodeUrl,
      lastSeen: new Date(),
    };

    this.addNode(node);
    return node;
  }

  /**
   * Add a node to the routing table.
   */
  addNode(node: DHTNode): boolean {
    if (node.id === this.identity.amid) return false;

    const bucketIndex = getBucketIndex(this.identity.amid, node.id);

    let bucket = this.buckets.get(bucketIndex);
    if (!bucket) {
      bucket = new KBucket(this.k);
      this.buckets.set(bucketIndex, bucket);
    }

    const added = bucket.addOrUpdate(node);
    if (added) {
      this.updateMetrics();
    }

    return added;
  }

  /**
   * Store a value in the DHT.
   */
  async put(
    key: string,
    value: Uint8Array,
    ttlSeconds: number = 3600
  ): Promise<void> {
    // Sign the value
    const signature = await this.identity.sign(value);

    const entry: DHTEntry = {
      key,
      value,
      publisher: this.identity.amid,
      signature,
      expiresAt: new Date(Date.now() + ttlSeconds * 1000),
      updatedAt: new Date(),
    };

    // Store locally
    this.storage.set(key, entry);

    // In a real implementation, would replicate to k closest nodes
    // For now, just store locally
    this.metrics.storedEntries = this.storage.size;
  }

  /**
   * Retrieve a value from the DHT.
   */
  async get(key: string): Promise<DHTEntry | null> {
    const startTime = Date.now();
    this.metrics.lookups++;

    // Check local storage first
    const local = this.storage.get(key);
    if (local) {
      // Check expiration
      if (local.expiresAt > new Date()) {
        this.updateLookupTime(startTime);
        return local;
      }
      // Expired
      this.storage.delete(key);
    }

    // In a real implementation, would query k closest nodes
    // For now, return null (not found)
    this.metrics.failedLookups++;
    return null;
  }

  /**
   * Register an agent with capabilities.
   */
  async registerAgent(capabilities: string[]): Promise<void> {
    const timestamp = Date.now();
    const data = JSON.stringify({
      amid: this.identity.amid,
      capabilities,
      timestamp,
    });

    const signature = await this.identity.sign(new TextEncoder().encode(data));

    const entry: DHTCapabilityEntry = {
      amid: this.identity.amid,
      capabilities,
      address: '', // Would be set by caller
      timestamp,
      signature: btoa(String.fromCharCode(...signature)),
    };

    // Store under each capability key
    for (const capability of capabilities) {
      const key = `capability:${capability}`;
      const value = new TextEncoder().encode(JSON.stringify(entry));
      await this.put(key, value, 3600); // 1 hour TTL
    }
  }

  /**
   * Find agents with a specific capability.
   */
  async findAgents(capability: string): Promise<DHTCapabilityEntry[]> {
    const key = `capability:${capability}`;
    const entry = await this.get(key);

    if (!entry) {
      return [];
    }

    try {
      const data = JSON.parse(new TextDecoder().decode(entry.value));
      return [data as DHTCapabilityEntry];
    } catch {
      return [];
    }
  }

  /**
   * Perform iterative lookup for a key.
   */
  async iterativeLookup(targetId: string): Promise<DHTNode[]> {
    // Get closest nodes we know
    const closest = this.getClosestNodes(targetId, this.alpha);

    // In a real implementation, would iteratively query nodes
    // For now, return what we have
    return closest;
  }

  /**
   * Get the k closest nodes to a target.
   */
  getClosestNodes(targetId: string, count: number = this.k): DHTNode[] {
    const allNodes: DHTNode[] = [];

    for (const bucket of this.buckets.values()) {
      allNodes.push(...bucket.getNodes());
    }

    // Sort by distance to target
    allNodes.sort((a, b) => {
      const distA = xorDistance(targetId, a.id);
      const distB = xorDistance(targetId, b.id);
      return compareDistance(distA, distB);
    });

    return allNodes.slice(0, count);
  }

  /**
   * Get the number of known nodes.
   */
  getKnownNodesCount(): number {
    let count = 0;
    for (const bucket of this.buckets.values()) {
      count += bucket.size;
    }
    return count;
  }

  /**
   * Get DHT metrics.
   */
  getMetrics(): DHTMetrics {
    return { ...this.metrics };
  }

  /**
   * Clear all stored data and nodes.
   */
  clear(): void {
    this.buckets.clear();
    this.storage.clear();
    this.metrics = {
      knownNodes: 0,
      storedEntries: 0,
      lookups: 0,
      avgLookupTimeMs: 0,
      failedLookups: 0,
    };
  }

  /**
   * Hash a URL to a node ID (for testing).
   */
  private hashUrl(url: string): string {
    // Simple hash for testing
    let hash = 0;
    for (const char of url) {
      hash = ((hash << 5) - hash) + char.charCodeAt(0);
      hash = hash & hash;
    }
    return Math.abs(hash).toString(16).padStart(16, '0');
  }

  /**
   * Update metrics.
   */
  private updateMetrics(): void {
    this.metrics.knownNodes = this.getKnownNodesCount();
    this.metrics.storedEntries = this.storage.size;
  }

  /**
   * Update average lookup time.
   */
  private updateLookupTime(startTime: number): void {
    const lookupTime = Date.now() - startTime;
    const totalLookups = this.metrics.lookups;

    // Running average
    this.metrics.avgLookupTimeMs =
      (this.metrics.avgLookupTimeMs * (totalLookups - 1) + lookupTime) / totalLookups;
  }
}

/**
 * Create a DHT key from a capability.
 */
export function createCapabilityKey(capability: string): string {
  return `cap:${capability}`;
}

/**
 * Create a DHT key from an AMID.
 */
export function createAmidKey(amid: string): string {
  return `agent:${amid}`;
}
