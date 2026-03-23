/**
 * Certificates module for AgentMesh.
 * Handles X.509 certificate parsing, chain building, and validation.
 */

import { CryptoError, ValidationError } from '../errors';

/**
 * Parsed X.509 certificate.
 */
export interface Certificate {
  /** Raw DER-encoded certificate */
  raw: Uint8Array;
  /** Subject distinguished name */
  subject: string;
  /** Issuer distinguished name */
  issuer: string;
  /** Serial number */
  serialNumber: string;
  /** Not valid before date */
  notBefore: Date;
  /** Not valid after date */
  notAfter: Date;
  /** Subject public key */
  publicKey: Uint8Array;
  /** Signature algorithm */
  signatureAlgorithm: string;
  /** Signature value */
  signature: Uint8Array;
  /** Is this a CA certificate? */
  isCA: boolean;
  /** Key usage extensions */
  keyUsage: string[];
  /** Extended key usage extensions */
  extKeyUsage: string[];
}

/**
 * Certificate chain validation result.
 */
export interface ChainValidationResult {
  /** Is the chain valid? */
  valid: boolean;
  /** Error message if invalid */
  error?: string;
  /** Validated certificate chain (leaf to root) */
  chain: Certificate[];
}

/**
 * Trusted root certificates for chain validation.
 */
export interface TrustStore {
  /** List of trusted root certificates */
  roots: Certificate[];
  /** Add a trusted root certificate */
  addRoot(cert: Certificate): void;
  /** Check if a certificate is trusted */
  isTrusted(cert: Certificate): boolean;
}

/**
 * Helper to convert ArrayBuffer to Uint8Array.
 */
function toArrayBuffer(data: Uint8Array): ArrayBuffer {
  const buffer = new ArrayBuffer(data.length);
  new Uint8Array(buffer).set(data);
  return buffer;
}

/**
 * Parse PEM-encoded certificate to DER.
 */
export function parsePEM(pem: string): Uint8Array {
  const lines = pem.split('\n');
  const base64Lines: string[] = [];
  let inCert = false;

  for (const line of lines) {
    if (line.includes('-----BEGIN CERTIFICATE-----')) {
      inCert = true;
      continue;
    }
    if (line.includes('-----END CERTIFICATE-----')) {
      break;
    }
    if (inCert && line.trim()) {
      base64Lines.push(line.trim());
    }
  }

  const base64 = base64Lines.join('');
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

/**
 * Convert DER to PEM format.
 */
export function toPEM(der: Uint8Array): string {
  const binary = String.fromCharCode(...der);
  const base64 = btoa(binary);
  const lines: string[] = [];
  for (let i = 0; i < base64.length; i += 64) {
    lines.push(base64.slice(i, i + 64));
  }
  return `-----BEGIN CERTIFICATE-----\n${lines.join('\n')}\n-----END CERTIFICATE-----`;
}

/**
 * Parse a simplified X.509 certificate from DER.
 * Note: This is a simplified parser for AgentMesh certificates.
 * For full X.509 parsing, consider using @peculiar/x509.
 */
export function parseCertificate(der: Uint8Array): Certificate {
  // Simplified parsing - extract key fields
  // In practice, X.509 parsing requires ASN.1 DER decoding
  // This implementation provides a structure for the certificate module

  // For now, we'll use a basic structure
  // A full implementation would use ASN.1 DER parsing

  // Basic validation - must start with SEQUENCE tag (0x30)
  if (der[0] !== 0x30) {
    throw new ValidationError('Invalid certificate format: expected SEQUENCE');
  }

  // Extract what we can from the raw bytes
  // This is a placeholder - real implementation needs ASN.1 parser
  const now = new Date();

  return {
    raw: der,
    subject: extractSubject(der),
    issuer: extractIssuer(der),
    serialNumber: extractSerialNumber(der),
    notBefore: new Date(now.getTime() - 365 * 24 * 60 * 60 * 1000), // 1 year ago
    notAfter: new Date(now.getTime() + 365 * 24 * 60 * 60 * 1000), // 1 year from now
    publicKey: extractPublicKey(der),
    signatureAlgorithm: 'Ed25519',
    signature: extractSignature(der),
    isCA: false,
    keyUsage: ['digitalSignature'],
    extKeyUsage: [],
  };
}

/**
 * Extract subject from DER - simplified implementation.
 */
function extractSubject(der: Uint8Array): string {
  // Simplified - return hash of certificate
  return `CN=${bytesToHex(der.slice(4, 12))}`;
}

/**
 * Extract issuer from DER - simplified implementation.
 */
function extractIssuer(der: Uint8Array): string {
  // Simplified - return hash of certificate
  return `CN=${bytesToHex(der.slice(12, 20))}`;
}

/**
 * Extract serial number from DER - simplified implementation.
 */
function extractSerialNumber(der: Uint8Array): string {
  // Simplified - use first 8 bytes as serial
  return bytesToHex(der.slice(0, 8));
}

/**
 * Extract public key from DER - simplified implementation.
 */
function extractPublicKey(der: Uint8Array): Uint8Array {
  // Look for public key bytes (32 bytes for Ed25519)
  // This is a placeholder - real implementation needs ASN.1 parsing
  if (der.length >= 64) {
    return der.slice(der.length - 64, der.length - 32);
  }
  return new Uint8Array(32);
}

/**
 * Extract signature from DER - simplified implementation.
 */
function extractSignature(der: Uint8Array): Uint8Array {
  // Last 64 bytes are typically the signature for Ed25519
  if (der.length >= 64) {
    return der.slice(der.length - 64);
  }
  return new Uint8Array(64);
}

/**
 * Convert bytes to hex string.
 */
function bytesToHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map(b => b.toString(16).padStart(2, '0'))
    .join('');
}

/**
 * Certificate manager for building and validating chains.
 */
export class CertificateManager {
  private trustedRoots: Map<string, Certificate> = new Map();

  /**
   * Add a trusted root certificate.
   */
  addTrustedRoot(cert: Certificate): void {
    const key = this.getCertificateKey(cert);
    this.trustedRoots.set(key, cert);
  }

  /**
   * Add multiple trusted root certificates.
   */
  addTrustedRoots(certs: Certificate[]): void {
    for (const cert of certs) {
      this.addTrustedRoot(cert);
    }
  }

  /**
   * Check if a certificate is a trusted root.
   */
  isTrustedRoot(cert: Certificate): boolean {
    const key = this.getCertificateKey(cert);
    return this.trustedRoots.has(key);
  }

  /**
   * Get a unique key for a certificate.
   */
  private getCertificateKey(cert: Certificate): string {
    return `${cert.subject}:${cert.serialNumber}`;
  }

  /**
   * Build a certificate chain from leaf to root.
   */
  buildChain(leafCert: Certificate, intermediateCerts: Certificate[]): Certificate[] {
    const chain: Certificate[] = [leafCert];
    let current = leafCert;

    // Try to build chain to a root
    while (!this.isTrustedRoot(current) && current.subject !== current.issuer) {
      // Find issuer in intermediates
      const issuer = intermediateCerts.find(c => c.subject === current.issuer);
      if (!issuer) {
        break;
      }
      chain.push(issuer);
      current = issuer;
    }

    return chain;
  }

  /**
   * Validate a certificate chain.
   */
  async validateChain(chain: Certificate[]): Promise<ChainValidationResult> {
    if (chain.length === 0) {
      return { valid: false, error: 'Empty certificate chain', chain: [] };
    }

    // Check expiration of all certificates
    const now = new Date();
    for (const cert of chain) {
      if (!this.isValidTime(cert, now)) {
        return {
          valid: false,
          error: `Certificate expired or not yet valid: ${cert.subject}`,
          chain: [],
        };
      }
    }

    // Check chain linkage
    for (let i = 0; i < chain.length - 1; i++) {
      const current = chain[i]!;
      const issuer = chain[i + 1]!;

      if (current.issuer !== issuer.subject) {
        return {
          valid: false,
          error: `Chain broken: ${current.subject} issuer does not match ${issuer.subject}`,
          chain: [],
        };
      }

      // Verify signature
      const signatureValid = await this.verifySignature(current, issuer);
      if (!signatureValid) {
        return {
          valid: false,
          error: `Invalid signature on certificate: ${current.subject}`,
          chain: [],
        };
      }
    }

    // Check if chain ends at trusted root
    const lastCert = chain[chain.length - 1]!;
    if (!this.isTrustedRoot(lastCert) && lastCert.subject !== lastCert.issuer) {
      return {
        valid: false,
        error: 'Chain does not end at trusted root',
        chain: [],
      };
    }

    // For self-signed root, verify self-signature
    if (lastCert.subject === lastCert.issuer) {
      const selfSignatureValid = await this.verifySignature(lastCert, lastCert);
      if (!selfSignatureValid) {
        return {
          valid: false,
          error: 'Invalid self-signature on root certificate',
          chain: [],
        };
      }
    }

    return { valid: true, chain };
  }

  /**
   * Check if a certificate is valid at a given time.
   */
  isValidTime(cert: Certificate, time: Date = new Date()): boolean {
    return time >= cert.notBefore && time <= cert.notAfter;
  }

  /**
   * Check if a certificate is expired.
   */
  isExpired(cert: Certificate): boolean {
    return new Date() > cert.notAfter;
  }

  /**
   * Verify certificate signature using issuer's public key.
   */
  async verifySignature(cert: Certificate, issuer: Certificate): Promise<boolean> {
    try {
      // Import issuer's public key
      const publicKey = await crypto.subtle.importKey(
        'raw',
        toArrayBuffer(issuer.publicKey),
        { name: 'Ed25519' } as Algorithm,
        false,
        ['verify']
      );

      // For Ed25519, we need to verify the TBS (To Be Signed) part
      // In a real implementation, we'd extract TBS from ASN.1
      // For now, use the raw certificate minus signature
      const tbsLength = cert.raw.length - cert.signature.length;
      const tbs = cert.raw.slice(0, Math.max(0, tbsLength));

      const result = await crypto.subtle.verify(
        { name: 'Ed25519' } as Algorithm,
        publicKey,
        toArrayBuffer(cert.signature),
        toArrayBuffer(tbs)
      );

      return result;
    } catch {
      // If verification fails, return false
      return false;
    }
  }

  /**
   * Get all trusted root certificates.
   */
  getTrustedRoots(): Certificate[] {
    return Array.from(this.trustedRoots.values());
  }

  /**
   * Clear all trusted roots.
   */
  clearTrustedRoots(): void {
    this.trustedRoots.clear();
  }
}

/**
 * Create a simple in-memory trust store.
 */
export function createTrustStore(): TrustStore {
  const roots: Certificate[] = [];

  return {
    roots,
    addRoot(cert: Certificate): void {
      roots.push(cert);
    },
    isTrusted(cert: Certificate): boolean {
      return roots.some(
        r => r.subject === cert.subject && r.serialNumber === cert.serialNumber
      );
    },
  };
}
