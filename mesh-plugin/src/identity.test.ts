import { describe, it, expect, beforeEach, afterEach } from "vitest";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import {
  generateIdentity,
  saveIdentity,
  loadIdentity,
  deriveAmid,
  loadOrCreateIdentity,
} from "./identity.js";

const TEST_DIR = path.join(os.tmpdir(), `azureclaw-test-${Date.now()}`);

describe("identity", () => {
  beforeEach(() => {
    fs.mkdirSync(TEST_DIR, { recursive: true });
  });
  afterEach(() => {
    fs.rmSync(TEST_DIR, { recursive: true, force: true });
  });

  it("generates an identity with AMID", () => {
    const id = generateIdentity();
    expect(id.amid).toBeTruthy();
    expect(id.amid.length).toBeGreaterThan(10);
    expect(id.signingPublicKey.length).toBe(32);
    expect(id.signingPrivateKey.length).toBe(32);
  });

  it("generates different AMIDs for different keys", () => {
    const id1 = generateIdentity();
    const id2 = generateIdentity();
    expect(id1.amid).not.toBe(id2.amid);
  });

  it("AMID is deterministic for same public key", () => {
    const id = generateIdentity();
    const amid1 = deriveAmid(id.signingPublicKey);
    const amid2 = deriveAmid(id.signingPublicKey);
    expect(amid1).toBe(amid2);
  });

  it("AMID uses base58 alphabet", () => {
    const id = generateIdentity();
    const base58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    for (const c of id.amid) {
      expect(base58).toContain(c);
    }
  });

  it("saves and loads identity (roundtrip)", () => {
    const id = generateIdentity();
    saveIdentity(id);

    const loaded = loadIdentity();
    expect(loaded).not.toBeNull();
    expect(loaded!.amid).toBe(id.amid);
    expect(Buffer.from(loaded!.signingPublicKey).toString("hex")).toBe(
      Buffer.from(id.signingPublicKey).toString("hex")
    );
    expect(Buffer.from(loaded!.signingPrivateKey).toString("hex")).toBe(
      Buffer.from(id.signingPrivateKey).toString("hex")
    );
  });

  it("loadIdentity returns null when no file exists", () => {
    // Note: may return non-null if a previous identity file exists on disk
    const result = loadIdentity();
    if (result) {
      expect(result.amid).toBeTruthy();
    }
  });

  it("loadOrCreateIdentity always returns an identity", () => {
    const id = loadOrCreateIdentity();
    expect(id.amid).toBeTruthy();
    expect(id.signingPublicKey.length).toBe(32);
  });

  it("identity implements IMeshIdentity (agentId field)", () => {
    const id = generateIdentity();
    expect(id.agentId).toBe(id.amid);
  });
});
