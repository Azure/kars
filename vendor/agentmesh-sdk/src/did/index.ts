/**
 * DID (Decentralized Identifier) module for AgentMesh.
 * Implements DID document creation, signing, and verification.
 */

import { Identity } from '../identity';
import { ValidationError } from '../errors';

/**
 * DID Document verification method.
 */
export interface VerificationMethod {
  /** Verification method ID */
  id: string;
  /** Type of verification method */
  type: string;
  /** Controller DID */
  controller: string;
  /** Public key in multibase format */
  publicKeyMultibase?: string;
  /** Public key as JWK */
  publicKeyJwk?: Record<string, string>;
}

/**
 * DID Document service endpoint.
 */
export interface ServiceEndpoint {
  /** Service ID */
  id: string;
  /** Service type */
  type: string;
  /** Service endpoint URL or object */
  serviceEndpoint: string | Record<string, unknown>;
}

/**
 * DID Document structure.
 */
export interface DIDDocument {
  /** JSON-LD context */
  '@context': string | string[];
  /** DID identifier */
  id: string;
  /** Alternative identifiers */
  alsoKnownAs?: string[];
  /** Controller DIDs */
  controller?: string | string[];
  /** Verification methods */
  verificationMethod?: VerificationMethod[];
  /** Authentication methods */
  authentication?: (string | VerificationMethod)[];
  /** Assertion methods */
  assertionMethod?: (string | VerificationMethod)[];
  /** Key agreement methods */
  keyAgreement?: (string | VerificationMethod)[];
  /** Service endpoints */
  service?: ServiceEndpoint[];
  /** Document metadata */
  metadata?: DIDDocumentMetadata;
}

/**
 * DID Document metadata.
 */
export interface DIDDocumentMetadata {
  /** Creation timestamp */
  created?: string;
  /** Last update timestamp */
  updated?: string;
  /** Is document deactivated? */
  deactivated?: boolean;
  /** Version ID */
  versionId?: string;
  /** Next update timestamp */
  nextUpdate?: string;
}

/**
 * Signed DID Document.
 */
export interface SignedDIDDocument {
  /** The DID document */
  document: DIDDocument;
  /** Proof/signature */
  proof: {
    /** Proof type */
    type: string;
    /** Creation timestamp */
    created: string;
    /** Verification method used */
    verificationMethod: string;
    /** Signature purpose */
    proofPurpose: string;
    /** Signature value */
    proofValue: string;
  };
}

/**
 * DID resolution result.
 */
export interface DIDResolutionResult {
  /** The resolved DID document */
  didDocument: DIDDocument | null;
  /** Resolution metadata */
  didResolutionMetadata: {
    error?: string;
    errorMessage?: string;
    contentType?: string;
  };
  /** Document metadata */
  didDocumentMetadata: DIDDocumentMetadata;
}

/**
 * Helper to convert Uint8Array to multibase (base58btc).
 */
function toMultibase(bytes: Uint8Array): string {
  // Base58 alphabet
  const ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';

  // Convert bytes to base58
  let num = 0n;
  for (const byte of bytes) {
    num = num * 256n + BigInt(byte);
  }

  let base58 = '';
  while (num > 0n) {
    const remainder = Number(num % 58n);
    num = num / 58n;
    base58 = ALPHABET[remainder] + base58;
  }

  // Handle leading zeros
  for (const byte of bytes) {
    if (byte === 0) {
      base58 = '1' + base58;
    } else {
      break;
    }
  }

  // 'z' prefix for base58btc multibase
  return 'z' + base58;
}

/**
 * Convert Uint8Array to ArrayBuffer for Web Crypto API compatibility.
 */
function toArrayBuffer(data: Uint8Array): ArrayBuffer {
  const buffer = new ArrayBuffer(data.length);
  new Uint8Array(buffer).set(data);
  return buffer;
}

/**
 * Decode multibase to bytes.
 */
function fromMultibase(multibase: string): Uint8Array {
  const ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';

  // Check for base58btc prefix
  if (!multibase.startsWith('z')) {
    throw new ValidationError('Unsupported multibase format');
  }

  const base58 = multibase.slice(1);

  // Decode base58
  let num = 0n;
  for (const char of base58) {
    const index = ALPHABET.indexOf(char);
    if (index === -1) {
      throw new ValidationError('Invalid base58 character');
    }
    num = num * 58n + BigInt(index);
  }

  // Convert to bytes
  const bytes: number[] = [];
  while (num > 0n) {
    bytes.unshift(Number(num % 256n));
    num = num / 256n;
  }

  // Handle leading '1's (zeros)
  for (const char of base58) {
    if (char === '1') {
      bytes.unshift(0);
    } else {
      break;
    }
  }

  return new Uint8Array(bytes);
}

/**
 * DID Manager for creating and managing DID documents.
 */
export class DIDManager {
  private static readonly CONTEXT = [
    'https://www.w3.org/ns/did/v1',
    'https://w3id.org/security/suites/ed25519-2020/v1',
    'https://w3id.org/security/suites/x25519-2020/v1',
  ];

  /**
   * Create a DID from an AMID.
   */
  static createDID(amid: string): string {
    return `did:agentmesh:${amid}`;
  }

  /**
   * Extract AMID from a DID.
   */
  static extractAmid(did: string): string | null {
    const match = did.match(/^did:agentmesh:(.+)$/);
    return match ? match[1]! : null;
  }

  /**
   * Create a DID Document from an identity.
   */
  static createDocument(identity: Identity, options?: {
    serviceEndpoints?: ServiceEndpoint[];
    alsoKnownAs?: string[];
    controller?: string;
  }): DIDDocument {
    const did = this.createDID(identity.amid);

    // Create verification methods
    const signingVerificationMethod: VerificationMethod = {
      id: `${did}#signing-key`,
      type: 'Ed25519VerificationKey2020',
      controller: did,
      publicKeyMultibase: toMultibase(identity.getSigningPublicKeyRaw()),
    };

    const keyAgreementMethod: VerificationMethod = {
      id: `${did}#key-agreement`,
      type: 'X25519KeyAgreementKey2020',
      controller: did,
      publicKeyMultibase: toMultibase(identity.getExchangePublicKeyRaw()),
    };

    const document: DIDDocument = {
      '@context': this.CONTEXT,
      id: did,
      verificationMethod: [signingVerificationMethod, keyAgreementMethod],
      authentication: [`${did}#signing-key`],
      assertionMethod: [`${did}#signing-key`],
      keyAgreement: [`${did}#key-agreement`],
      metadata: {
        created: new Date().toISOString(),
      },
    };

    // Add optional fields
    if (options?.alsoKnownAs) {
      document.alsoKnownAs = options.alsoKnownAs;
    }

    if (options?.controller) {
      document.controller = options.controller;
    }

    if (options?.serviceEndpoints) {
      document.service = options.serviceEndpoints;
    }

    return document;
  }

  /**
   * Sign a DID document.
   */
  static async signDocument(
    document: DIDDocument,
    identity: Identity
  ): Promise<SignedDIDDocument> {
    // Serialize document for signing (canonicalized)
    const documentJson = JSON.stringify(document, Object.keys(document).sort());
    const documentBytes = new TextEncoder().encode(documentJson);

    // Sign the document
    const signature = await identity.sign(documentBytes);

    return {
      document,
      proof: {
        type: 'Ed25519Signature2020',
        created: new Date().toISOString(),
        verificationMethod: `${document.id}#signing-key`,
        proofPurpose: 'assertionMethod',
        proofValue: toMultibase(signature),
      },
    };
  }

  /**
   * Verify a signed DID document.
   */
  static async verifyDocument(
    signedDocument: SignedDIDDocument,
    publicKey?: Uint8Array
  ): Promise<{ valid: boolean; error?: string }> {
    const { document, proof } = signedDocument;

    // Get public key from document if not provided
    let verifyKey = publicKey;
    if (!verifyKey) {
      const verificationMethod = document.verificationMethod?.find(
        vm => vm.id === proof.verificationMethod
      );
      if (!verificationMethod?.publicKeyMultibase) {
        return { valid: false, error: 'Cannot find verification method' };
      }
      verifyKey = fromMultibase(verificationMethod.publicKeyMultibase);
    }

    // Serialize document for verification
    const documentJson = JSON.stringify(document, Object.keys(document).sort());
    const documentBytes = new TextEncoder().encode(documentJson);

    // Decode signature
    const signature = fromMultibase(proof.proofValue);

    try {
      // Import public key
      const key = await crypto.subtle.importKey(
        'raw',
        toArrayBuffer(verifyKey),
        { name: 'Ed25519' } as Algorithm,
        false,
        ['verify']
      );

      // Verify signature
      const valid = await crypto.subtle.verify(
        { name: 'Ed25519' } as Algorithm,
        key,
        toArrayBuffer(signature),
        toArrayBuffer(documentBytes)
      );

      return { valid };
    } catch (error) {
      return { valid: false, error: String(error) };
    }
  }

  /**
   * Add a service endpoint to a document.
   */
  static addService(document: DIDDocument, service: ServiceEndpoint): DIDDocument {
    const updated = { ...document };
    updated.service = [...(updated.service || []), service];
    if (updated.metadata) {
      updated.metadata.updated = new Date().toISOString();
    }
    return updated;
  }

  /**
   * Remove a service endpoint from a document.
   */
  static removeService(document: DIDDocument, serviceId: string): DIDDocument {
    const updated = { ...document };
    updated.service = (updated.service || []).filter(s => s.id !== serviceId);
    if (updated.metadata) {
      updated.metadata.updated = new Date().toISOString();
    }
    return updated;
  }

  /**
   * Deactivate a DID document.
   */
  static deactivate(document: DIDDocument): DIDDocument {
    const updated = { ...document };
    updated.metadata = {
      ...updated.metadata,
      deactivated: true,
      updated: new Date().toISOString(),
    };
    return updated;
  }

  /**
   * Serialize a DID document to JSON-LD.
   */
  static serialize(document: DIDDocument): string {
    return JSON.stringify(document, null, 2);
  }

  /**
   * Deserialize a DID document from JSON.
   */
  static deserialize(json: string): DIDDocument {
    const doc = JSON.parse(json) as DIDDocument;

    // Basic validation
    if (!doc.id || !doc['@context']) {
      throw new ValidationError('Invalid DID document: missing required fields');
    }

    if (!doc.id.startsWith('did:')) {
      throw new ValidationError('Invalid DID format');
    }

    return doc;
  }
}

/**
 * DID Resolver for looking up and caching DID documents.
 */
export class DIDResolver {
  private cache: Map<string, { document: DIDDocument; timestamp: number }> = new Map();
  private cacheTtlMs: number;

  constructor(options?: { cacheTtlMs?: number }) {
    this.cacheTtlMs = options?.cacheTtlMs || 5 * 60 * 1000; // 5 minutes default
  }

  /**
   * Resolve a DID to its document.
   * This is a local resolver - for AgentMesh DIDs, we need the registry.
   */
  async resolve(did: string): Promise<DIDResolutionResult> {
    // Check cache
    const cached = this.cache.get(did);
    if (cached && Date.now() - cached.timestamp < this.cacheTtlMs) {
      return {
        didDocument: cached.document,
        didResolutionMetadata: { contentType: 'application/did+ld+json' },
        didDocumentMetadata: cached.document.metadata || {},
      };
    }

    // Parse the DID
    const match = did.match(/^did:([^:]+):(.+)$/);
    if (!match) {
      return {
        didDocument: null,
        didResolutionMetadata: {
          error: 'invalidDid',
          errorMessage: 'Invalid DID format',
        },
        didDocumentMetadata: {},
      };
    }

    const [, method, identifier] = match;

    // For agentmesh method, we need to look up in registry
    if (method === 'agentmesh') {
      // This would be integrated with the registry client
      return {
        didDocument: null,
        didResolutionMetadata: {
          error: 'notFound',
          errorMessage: 'DID document not found. Use registry lookup.',
        },
        didDocumentMetadata: {},
      };
    }

    // Unknown method
    return {
      didDocument: null,
      didResolutionMetadata: {
        error: 'methodNotSupported',
        errorMessage: `DID method '${method}' is not supported`,
      },
      didDocumentMetadata: {},
    };
  }

  /**
   * Cache a resolved document.
   */
  cacheDocument(did: string, document: DIDDocument): void {
    this.cache.set(did, { document, timestamp: Date.now() });
  }

  /**
   * Invalidate a cached document.
   */
  invalidate(did: string): void {
    this.cache.delete(did);
  }

  /**
   * Clear the entire cache.
   */
  clearCache(): void {
    this.cache.clear();
  }

  /**
   * Get cache statistics.
   */
  getCacheStats(): { size: number; hits: number; misses: number } {
    return {
      size: this.cache.size,
      hits: 0, // Would need to track in resolve()
      misses: 0,
    };
  }
}

/**
 * Create a relay service endpoint.
 */
export function createRelayServiceEndpoint(
  did: string,
  relayUrl: string
): ServiceEndpoint {
  return {
    id: `${did}#relay`,
    type: 'AgentMeshRelay',
    serviceEndpoint: relayUrl,
  };
}

/**
 * Create a DHT service endpoint.
 */
export function createDHTServiceEndpoint(
  did: string,
  dhtNode: string
): ServiceEndpoint {
  return {
    id: `${did}#dht`,
    type: 'AgentMeshDHT',
    serviceEndpoint: dhtNode,
  };
}
