/**
 * Unit tests for DID module.
 */
import { describe, test, expect, beforeEach } from 'vitest';
import {
  DIDManager,
  DIDResolver,
  createRelayServiceEndpoint,
  createDHTServiceEndpoint,
} from '../../src/did';
import { Identity } from '../../src/identity';

describe('DID', () => {
  let identity: Identity;

  beforeEach(async () => {
    identity = await Identity.generate();
  });

  describe('DIDManager', () => {
    test('should create DID from AMID', () => {
      const did = DIDManager.createDID('test-amid-12345');
      expect(did).toBe('did:agentmesh:test-amid-12345');
    });

    test('should extract AMID from DID', () => {
      const amid = DIDManager.extractAmid('did:agentmesh:test-amid-12345');
      expect(amid).toBe('test-amid-12345');
    });

    test('should return null for invalid DID', () => {
      const amid = DIDManager.extractAmid('not-a-valid-did');
      expect(amid).toBeNull();
    });

    test('should create DID document from identity', () => {
      const doc = DIDManager.createDocument(identity);

      expect(doc.id).toBe(`did:agentmesh:${identity.amid}`);
      expect(doc['@context']).toBeDefined();
      expect(doc.verificationMethod).toBeDefined();
      expect(doc.verificationMethod?.length).toBe(2);
      expect(doc.authentication).toBeDefined();
      expect(doc.keyAgreement).toBeDefined();
    });

    test('should create DID document with service endpoints', () => {
      const doc = DIDManager.createDocument(identity, {
        serviceEndpoints: [
          {
            id: `did:agentmesh:${identity.amid}#relay`,
            type: 'AgentMeshRelay',
            serviceEndpoint: 'wss://relay.agentmesh.ai',
          },
        ],
      });

      expect(doc.service).toBeDefined();
      expect(doc.service?.length).toBe(1);
      expect(doc.service?.[0]?.type).toBe('AgentMeshRelay');
    });

    test('should create DID document with alsoKnownAs', () => {
      const doc = DIDManager.createDocument(identity, {
        alsoKnownAs: ['https://example.com/agent'],
      });

      expect(doc.alsoKnownAs).toBeDefined();
      expect(doc.alsoKnownAs?.[0]).toBe('https://example.com/agent');
    });

    test('should sign DID document', async () => {
      const doc = DIDManager.createDocument(identity);
      const signed = await DIDManager.signDocument(doc, identity);

      expect(signed.document).toEqual(doc);
      expect(signed.proof).toBeDefined();
      expect(signed.proof.type).toBe('Ed25519Signature2020');
      expect(signed.proof.proofValue).toBeDefined();
    });

    test('should verify signed DID document', async () => {
      const doc = DIDManager.createDocument(identity);
      const signed = await DIDManager.signDocument(doc, identity);

      const result = await DIDManager.verifyDocument(signed);
      expect(result.valid).toBe(true);
    });

    test('should add service endpoint to document', () => {
      const doc = DIDManager.createDocument(identity);
      const updated = DIDManager.addService(doc, {
        id: `${doc.id}#messaging`,
        type: 'MessagingService',
        serviceEndpoint: 'https://messaging.example.com',
      });

      expect(updated.service?.length).toBe(1);
      expect(updated.metadata?.updated).toBeDefined();
    });

    test('should remove service endpoint from document', () => {
      let doc = DIDManager.createDocument(identity);
      doc = DIDManager.addService(doc, {
        id: `${doc.id}#messaging`,
        type: 'MessagingService',
        serviceEndpoint: 'https://messaging.example.com',
      });

      const updated = DIDManager.removeService(doc, `${doc.id}#messaging`);
      expect(updated.service?.length).toBe(0);
    });

    test('should deactivate DID document', () => {
      const doc = DIDManager.createDocument(identity);
      const deactivated = DIDManager.deactivate(doc);

      expect(deactivated.metadata?.deactivated).toBe(true);
    });

    test('should serialize DID document', () => {
      const doc = DIDManager.createDocument(identity);
      const json = DIDManager.serialize(doc);

      expect(typeof json).toBe('string');
      expect(json).toContain(doc.id);
    });

    test('should deserialize DID document', () => {
      const doc = DIDManager.createDocument(identity);
      const json = DIDManager.serialize(doc);
      const parsed = DIDManager.deserialize(json);

      expect(parsed.id).toBe(doc.id);
    });

    test('should throw on invalid DID document', () => {
      expect(() => DIDManager.deserialize('{}')).toThrow();
      expect(() => DIDManager.deserialize('{"id": "invalid"}')).toThrow();
    });
  });

  describe('DIDResolver', () => {
    test('should create resolver', () => {
      const resolver = new DIDResolver();
      expect(resolver).toBeDefined();
    });

    test('should cache DID document', async () => {
      const resolver = new DIDResolver();
      const doc = DIDManager.createDocument(identity);
      const did = `did:agentmesh:${identity.amid}`;

      resolver.cacheDocument(did, doc);
      const result = await resolver.resolve(did);

      expect(result.didDocument).toEqual(doc);
    });

    test('should invalidate cached document', async () => {
      const resolver = new DIDResolver();
      const doc = DIDManager.createDocument(identity);
      const did = `did:agentmesh:${identity.amid}`;

      resolver.cacheDocument(did, doc);
      resolver.invalidate(did);

      const result = await resolver.resolve(did);
      // Should return not found (cache invalidated, no network lookup)
      expect(result.didResolutionMetadata.error).toBe('notFound');
    });

    test('should clear cache', async () => {
      const resolver = new DIDResolver();
      const doc = DIDManager.createDocument(identity);
      const did = `did:agentmesh:${identity.amid}`;

      resolver.cacheDocument(did, doc);
      resolver.clearCache();

      const stats = resolver.getCacheStats();
      expect(stats.size).toBe(0);
    });

    test('should return error for invalid DID', async () => {
      const resolver = new DIDResolver();
      const result = await resolver.resolve('invalid');

      expect(result.didDocument).toBeNull();
      expect(result.didResolutionMetadata.error).toBe('invalidDid');
    });

    test('should return error for unsupported method', async () => {
      const resolver = new DIDResolver();
      const result = await resolver.resolve('did:unknown:12345');

      expect(result.didDocument).toBeNull();
      expect(result.didResolutionMetadata.error).toBe('methodNotSupported');
    });
  });

  describe('Service Endpoint Helpers', () => {
    test('should create relay service endpoint', () => {
      const did = 'did:agentmesh:test-amid';
      const endpoint = createRelayServiceEndpoint(did, 'wss://relay.example.com');

      expect(endpoint.id).toBe(`${did}#relay`);
      expect(endpoint.type).toBe('AgentMeshRelay');
      expect(endpoint.serviceEndpoint).toBe('wss://relay.example.com');
    });

    test('should create DHT service endpoint', () => {
      const did = 'did:agentmesh:test-amid';
      const endpoint = createDHTServiceEndpoint(did, 'dht.example.com:8080');

      expect(endpoint.id).toBe(`${did}#dht`);
      expect(endpoint.type).toBe('AgentMeshDHT');
      expect(endpoint.serviceEndpoint).toBe('dht.example.com:8080');
    });
  });
});
