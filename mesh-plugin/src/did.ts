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
 * without the agentId segment, since kars identities are key-derived,
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
 * raw key bytes (not SPKI-wrapped), matching kars's existing identity
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
 * Normalize any recognized DID to the canonical wire format.
 *
 * Canonical = the form the AGT mesh server stores and re-emits — which
 * since 2026-05-23 (server PR #2533) is `did:mesh:<sha256(pk)[:32]>`.
 * Earlier AGT releases used `did:agentmesh:<fingerprint>` which we
 * preserve for back-compat: any input that already parses cleanly is
 * returned in the same method+fingerprint form (we never silently
 * convert between methods because the fingerprints encode different
 * hash slices of different curves and aren't interchangeable).
 */
export function normalizeDid(did: string): string {
  const parsed = parseDid(did);
  if (!parsed) return did;

  if (parsed.method === "mesh") {
    return `did:mesh:${parsed.fingerprint}`;
  }
  if (parsed.method === "agentmesh") {
    return `did:agentmesh:${parsed.fingerprint}`;
  }
  return did;
}

/** Check if a string is a DID in any recognized format. */
export function isDid(value: string): boolean {
  return parseDid(value) !== null;
}

/**
 * Check if a string is the canonical wire DID.
 *
 * Both server-canonical `did:mesh:<32-hex>` (AGT main >= 2026-05-23)
 * and legacy `did:agentmesh:<16-hex>` are recognised so kars can talk
 * to either generation of relay without the caller having to branch.
 */
export function isCanonicalDid(value: string): boolean {
  return /^did:mesh:[0-9a-f]{32}$/.test(value)
    || /^did:agentmesh:[0-9a-f]{16}$/.test(value);
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
