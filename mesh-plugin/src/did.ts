// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
/**
 * DID canonicalization — deterministic derivation of AGT-compatible DIDs
 * from Ed25519 signing keys.
 *
 * Phase 3 of the AGT migration: adds canonical DID alongside existing AMID
 * so both identifiers are available during the cutover period.
 *
 * Canonical format: `did:agentmesh:<fingerprint>`
 *   where fingerprint = hex(sha256(raw_ed25519_pub))[:16]
 *
 * This is a simplified form of AGT's TS SDK format (`did:agentmesh:<agentId>:<fingerprint>`)
 * without the agentId segment, since AzureClaw identities are key-derived,
 * not name-derived. The full form is accepted as input and normalized.
 *
 * Accepted input formats (all normalized to canonical on parse):
 *   - `did:agentmesh:<fingerprint>`           (canonical)
 *   - `did:agentmesh:<agentId>:<fingerprint>`  (AGT TS SDK)
 *   - `did:mesh:<identifier>`                  (AGT Python SDK, passthrough)
 *   - AMID string                              (legacy, requires public key for DID)
 */

import * as crypto from "node:crypto";

// ---------------------------------------------------------------------------
// Canonical DID derivation
// ---------------------------------------------------------------------------

/**
 * Derive the canonical DID from a raw Ed25519 signing public key (32 bytes).
 *
 * Uses the same hash as AGT's TS SDK (`sha256(pubkey).hex[:16]`) but over
 * raw key bytes (not SPKI-wrapped), matching AzureClaw's existing identity
 * module. The fingerprint is self-verifying: anyone with the public key can
 * recompute it.
 */
export function deriveCanonicalDid(signingPublicKey: Buffer | Uint8Array): string {
  const fingerprint = crypto
    .createHash("sha256")
    .update(signingPublicKey)
    .digest("hex")
    .slice(0, 16);
  return `did:agentmesh:${fingerprint}`;
}

// ---------------------------------------------------------------------------
// DID parsing and normalization
// ---------------------------------------------------------------------------

export interface ParsedDid {
  /** The full DID string in its original form. */
  original: string;
  /** DID method (`agentmesh` or `mesh`). */
  method: "agentmesh" | "mesh";
  /** The fingerprint segment (last segment for agentmesh, identifier for mesh). */
  fingerprint: string;
  /** Optional agentId segment (present in AGT TS SDK `did:agentmesh:<agentId>:<fp>` form). */
  agentId?: string;
}

/**
 * Parse a DID string into its components.
 * Returns null if the string is not a recognized DID format.
 */
export function parseDid(did: string): ParsedDid | null {
  if (!did.startsWith("did:")) return null;

  // did:agentmesh:<fingerprint> or did:agentmesh:<agentId>:<fingerprint>
  const agentmeshMatch = did.match(/^did:agentmesh:([^:]+)(?::([^:]+))?$/);
  if (agentmeshMatch) {
    const [, first, second] = agentmeshMatch;
    if (second) {
      return { original: did, method: "agentmesh", agentId: first, fingerprint: second };
    }
    return { original: did, method: "agentmesh", fingerprint: first };
  }

  // did:mesh:<identifier>
  const meshMatch = did.match(/^did:mesh:(.+)$/);
  if (meshMatch) {
    return { original: did, method: "mesh", fingerprint: meshMatch[1] };
  }

  return null;
}

/**
 * Normalize any recognized DID to the canonical `did:agentmesh:<fingerprint>` form.
 * For `did:mesh:*` identifiers that aren't hex fingerprints, returns the original
 * since lossless normalization isn't possible without the public key.
 */
export function normalizeDid(did: string): string {
  const parsed = parseDid(did);
  if (!parsed) return did;

  if (parsed.method === "agentmesh") {
    return `did:agentmesh:${parsed.fingerprint}`;
  }

  // did:mesh: identifiers may use a different derivation, return as-is
  return did;
}

/** Check if a string is a DID in any recognized format. */
export function isDid(value: string): boolean {
  return parseDid(value) !== null;
}

/** Check if a string is the canonical `did:agentmesh:<fingerprint>` format. */
export function isCanonicalDid(value: string): boolean {
  return /^did:agentmesh:[0-9a-f]{16}$/.test(value);
}

/** Check if a string looks like a legacy AMID (base58, typically 24-28 chars). */
export function isAmid(value: string): boolean {
  return /^[123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz]{20,30}$/.test(value);
}

/**
 * Determine the type of a peer address string.
 * Useful for routing decisions during the dual-identity cutover period.
 */
export function classifyAddress(address: string): "canonical-did" | "did" | "amid" | "unknown" {
  if (isCanonicalDid(address)) return "canonical-did";
  if (isDid(address)) return "did";
  if (isAmid(address)) return "amid";
  return "unknown";
}
