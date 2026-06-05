// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
import { describe, it, expect } from "vitest";
import * as crypto from "node:crypto";
import {
  deriveCanonicalDid,
  parseDid,
  normalizeDid,
  isDid,
  isCanonicalDid,
  isAmid,
  classifyAddress,
} from "./did.js";

describe("deriveCanonicalDid", () => {
  it("produces did:mesh:<32-char-hex> from a 32-byte key", () => {
    const key = crypto.randomBytes(32);
    const did = deriveCanonicalDid(key);
    // Modern AGT format (matches @microsoft/agent-governance-sdk ≥4.0.0
    // and AGT Python registry post-PR #2533): did:mesh:<sha256[:32]>.
    expect(did).toMatch(/^did:mesh:[0-9a-f]{32}$/);
  });

  it("is deterministic for the same key", () => {
    const key = crypto.randomBytes(32);
    expect(deriveCanonicalDid(key)).toBe(deriveCanonicalDid(key));
  });

  it("produces different DIDs for different keys", () => {
    const did1 = deriveCanonicalDid(crypto.randomBytes(32));
    const did2 = deriveCanonicalDid(crypto.randomBytes(32));
    expect(did1).not.toBe(did2);
  });

  it("accepts Uint8Array input", () => {
    const key = new Uint8Array(crypto.randomBytes(32));
    const did = deriveCanonicalDid(key);
    expect(did).toMatch(/^did:mesh:[0-9a-f]{32}$/);
  });
});

describe("parseDid", () => {
  it("parses canonical did:agentmesh:<fingerprint>", () => {
    const result = parseDid("did:agentmesh:abcdef0123456789");
    expect(result).toEqual({
      original: "did:agentmesh:abcdef0123456789",
      method: "agentmesh",
      fingerprint: "abcdef0123456789",
    });
  });

  it("parses AGT TS SDK format did:agentmesh:<agentId>:<fingerprint>", () => {
    const result = parseDid("did:agentmesh:my-agent:abcdef0123456789");
    expect(result).toEqual({
      original: "did:agentmesh:my-agent:abcdef0123456789",
      method: "agentmesh",
      agentId: "my-agent",
      fingerprint: "abcdef0123456789",
    });
  });

  it("parses AGT Python SDK format did:mesh:<identifier>", () => {
    const result = parseDid("did:mesh:abc123def456");
    expect(result).toEqual({
      original: "did:mesh:abc123def456",
      method: "mesh",
      fingerprint: "abc123def456",
    });
  });

  it("returns null for non-DID strings", () => {
    expect(parseDid("not-a-did")).toBeNull();
    expect(parseDid("amid:something")).toBeNull();
    expect(parseDid("")).toBeNull();
  });

  it("returns null for malformed DIDs", () => {
    expect(parseDid("did:unknown:abc")).toBeNull();
    expect(parseDid("did:")).toBeNull();
  });
});

describe("normalizeDid", () => {
  it("returns canonical form unchanged", () => {
    expect(normalizeDid("did:agentmesh:abcdef0123456789")).toBe(
      "did:agentmesh:abcdef0123456789",
    );
  });

  it("strips agentId from AGT TS SDK format", () => {
    expect(normalizeDid("did:agentmesh:my-agent:abcdef0123456789")).toBe(
      "did:agentmesh:abcdef0123456789",
    );
  });

  it("preserves did:mesh: as-is (different derivation)", () => {
    expect(normalizeDid("did:mesh:some-id")).toBe("did:mesh:some-id");
  });

  it("returns non-DID strings unchanged", () => {
    expect(normalizeDid("not-a-did")).toBe("not-a-did");
  });
});

describe("isDid / isCanonicalDid / isAmid", () => {
  it("isDid recognizes all DID formats", () => {
    expect(isDid("did:agentmesh:abcdef0123456789")).toBe(true);
    expect(isDid("did:agentmesh:agent:abcdef0123456789")).toBe(true);
    expect(isDid("did:mesh:abc123")).toBe(true);
    expect(isDid("not-a-did")).toBe(false);
  });

  it("isCanonicalDid only matches did:agentmesh:<16-hex>", () => {
    expect(isCanonicalDid("did:agentmesh:abcdef0123456789")).toBe(true);
    expect(isCanonicalDid("did:agentmesh:agent:abcdef0123456789")).toBe(false);
    expect(isCanonicalDid("did:mesh:abcdef0123456789")).toBe(false);
    expect(isCanonicalDid("did:agentmesh:short")).toBe(false);
  });

  it("isAmid matches base58 strings of expected length", () => {
    expect(isAmid("2NEpo7TZRRrLZSi2U")).toBe(false); // too short
    expect(isAmid("2NEpo7TZRRrLZSi2U1234567")).toBe(true);
    expect(isAmid("did:agentmesh:abc")).toBe(false); // has colon
  });
});

describe("classifyAddress", () => {
  it("classifies canonical DID", () => {
    expect(classifyAddress("did:agentmesh:abcdef0123456789")).toBe("canonical-did");
  });

  it("classifies non-canonical DID", () => {
    expect(classifyAddress("did:mesh:something")).toBe("did");
    expect(classifyAddress("did:agentmesh:agent:abcdef0123456789")).toBe("did");
  });

  it("classifies AMID", () => {
    expect(classifyAddress("2NEpo7TZRRrLZSi2U1234567")).toBe("amid");
  });

  it("classifies unknown", () => {
    expect(classifyAddress("random")).toBe("unknown");
    expect(classifyAddress("")).toBe("unknown");
  });
});
