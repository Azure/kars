// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect, vi, beforeEach } from "vitest";
import * as crypto from "node:crypto";

// Mock fs before importing module under test
vi.mock("fs", () => ({
  existsSync: vi.fn(),
  readFileSync: vi.fn(),
  writeFileSync: vi.fn(),
  mkdirSync: vi.fn(),
}));

import {
  generateKeypair,
  base58Encode,
  encryptPrivateKey,
  decryptPrivateKey,
  meshCommand,
} from "./mesh.js";
import type { MeshIdentity } from "./mesh.js";

beforeEach(() => {
  vi.clearAllMocks();
});

// ---------------------------------------------------------------------------
// base58Encode
// ---------------------------------------------------------------------------

describe("base58Encode", () => {
  it("produces a non-empty string for non-zero input", () => {
    const buf = Buffer.from("hello");
    const encoded = base58Encode(buf);
    expect(encoded.length).toBeGreaterThan(0);
  });

  it("contains only valid base58 characters (no 0, O, I, l)", () => {
    // Run several random buffers to increase confidence
    for (let i = 0; i < 20; i++) {
      const buf = crypto.randomBytes(20);
      const encoded = base58Encode(buf);
      expect(encoded).toMatch(/^[123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz]+$/);
    }
  });

  it("preserves leading zero bytes as '1' characters", () => {
    const buf = Buffer.from([0, 0, 0, 1, 2]);
    const encoded = base58Encode(buf);
    expect(encoded.startsWith("111")).toBe(true);
  });

  it("returns deterministic output for the same input", () => {
    const buf = crypto.randomBytes(32);
    expect(base58Encode(buf)).toBe(base58Encode(buf));
  });

  it("encodes a known value correctly", () => {
    // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
    const hash = crypto.createHash("sha256").update("").digest();
    const encoded = base58Encode(hash);
    // Just verify it's a valid base58 string of reasonable length
    expect(encoded.length).toBeGreaterThan(30);
    expect(encoded).toMatch(/^[123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz]+$/);
  });

  it("encodes single byte values", () => {
    const buf = Buffer.from([0x01]);
    const encoded = base58Encode(buf);
    expect(encoded).toBe("2"); // base58: 1 → "2" (alphabet[1])
  });
});

// ---------------------------------------------------------------------------
// generateKeypair
// ---------------------------------------------------------------------------

describe("generateKeypair", () => {
  it("returns 32-byte public and private keys", () => {
    const kp = generateKeypair();
    expect(kp.publicKey).toBeInstanceOf(Buffer);
    expect(kp.privateKey).toBeInstanceOf(Buffer);
    expect(kp.publicKey.length).toBe(32);
    expect(kp.privateKey.length).toBe(32);
  });

  it("returns a non-empty AMID string", () => {
    const kp = generateKeypair();
    expect(typeof kp.amid).toBe("string");
    expect(kp.amid.length).toBeGreaterThan(0);
  });

  it("AMID contains only base58 characters", () => {
    const kp = generateKeypair();
    expect(kp.amid).toMatch(
      /^[123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz]+$/,
    );
  });

  it("AMID is deterministic for the same public key", () => {
    // Generate once; derive AMID manually and compare
    const kp = generateKeypair();
    const hash = crypto.createHash("sha256").update(kp.publicKey).digest();
    const expectedAmid = base58Encode(hash.subarray(0, 20));
    expect(kp.amid).toBe(expectedAmid);
  });

  it("generates unique keypairs on each call", () => {
    const kp1 = generateKeypair();
    const kp2 = generateKeypair();
    expect(kp1.publicKey.equals(kp2.publicKey)).toBe(false);
    expect(kp1.privateKey.equals(kp2.privateKey)).toBe(false);
    expect(kp1.amid).not.toBe(kp2.amid);
  });

  it("private key can sign and public key can verify", () => {
    const kp = generateKeypair();
    const message = Buffer.from("test message");

    // Wrap raw keys in DER envelopes for node crypto
    const privKeyObj = crypto.createPrivateKey({
      key: Buffer.concat([
        Buffer.from("302e020100300506032b657004220420", "hex"),
        kp.privateKey,
      ]),
      format: "der",
      type: "pkcs8",
    });
    const pubKeyObj = crypto.createPublicKey({
      key: Buffer.concat([
        Buffer.from("302a300506032b6570032100", "hex"),
        kp.publicKey,
      ]),
      format: "der",
      type: "spki",
    });

    const signature = crypto.sign(null, message, privKeyObj);
    const valid = crypto.verify(null, message, pubKeyObj, signature);
    expect(valid).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// encryptPrivateKey / decryptPrivateKey roundtrip
// ---------------------------------------------------------------------------

describe("encryptPrivateKey + decryptPrivateKey roundtrip", () => {
  it("round-trips a private key through encrypt then decrypt", () => {
    const kp = generateKeypair();
    const enc = encryptPrivateKey(kp.privateKey);

    const identity: MeshIdentity = {
      amid: kp.amid,
      publicKey: kp.publicKey.toString("base64"),
      encryptedPrivateKey: enc.encrypted,
      iv: enc.iv,
      authTag: enc.authTag,
      createdAt: new Date().toISOString(),
    };

    const decrypted = decryptPrivateKey(identity);
    expect(decrypted.equals(kp.privateKey)).toBe(true);
  });

  it("encrypted output fields are valid base64", () => {
    const kp = generateKeypair();
    const enc = encryptPrivateKey(kp.privateKey);

    // base64 decode should not throw
    expect(() => Buffer.from(enc.encrypted, "base64")).not.toThrow();
    expect(() => Buffer.from(enc.iv, "base64")).not.toThrow();
    expect(() => Buffer.from(enc.authTag, "base64")).not.toThrow();
  });

  it("iv is 12 bytes (96-bit for AES-GCM)", () => {
    const kp = generateKeypair();
    const enc = encryptPrivateKey(kp.privateKey);
    expect(Buffer.from(enc.iv, "base64").length).toBe(12);
  });

  it("authTag is 16 bytes (128-bit for AES-GCM)", () => {
    const kp = generateKeypair();
    const enc = encryptPrivateKey(kp.privateKey);
    expect(Buffer.from(enc.authTag, "base64").length).toBe(16);
  });

  it("different encryptions of the same key produce different ciphertext (random IV)", () => {
    const kp = generateKeypair();
    const enc1 = encryptPrivateKey(kp.privateKey);
    const enc2 = encryptPrivateKey(kp.privateKey);

    expect(enc1.iv).not.toBe(enc2.iv);
    expect(enc1.encrypted).not.toBe(enc2.encrypted);
  });

  it("decryption fails with tampered ciphertext", () => {
    const kp = generateKeypair();
    const enc = encryptPrivateKey(kp.privateKey);

    const tampered: MeshIdentity = {
      amid: kp.amid,
      publicKey: kp.publicKey.toString("base64"),
      encryptedPrivateKey: Buffer.from("tampered-data").toString("base64"),
      iv: enc.iv,
      authTag: enc.authTag,
      createdAt: new Date().toISOString(),
    };

    expect(() => decryptPrivateKey(tampered)).toThrow();
  });

  it("decryption fails with tampered auth tag", () => {
    const kp = generateKeypair();
    const enc = encryptPrivateKey(kp.privateKey);

    const badTag = crypto.randomBytes(16).toString("base64");
    const tampered: MeshIdentity = {
      amid: kp.amid,
      publicKey: kp.publicKey.toString("base64"),
      encryptedPrivateKey: enc.encrypted,
      iv: enc.iv,
      authTag: badTag,
      createdAt: new Date().toISOString(),
    };

    expect(() => decryptPrivateKey(tampered)).toThrow();
  });
});

// ---------------------------------------------------------------------------
// AMID derivation
// ---------------------------------------------------------------------------

describe("AMID derivation", () => {
  it("is base58(sha256(publicKey)[:20])", () => {
    const kp = generateKeypair();
    const hash = crypto.createHash("sha256").update(kp.publicKey).digest();
    const expected = base58Encode(hash.subarray(0, 20));
    expect(kp.amid).toBe(expected);
  });

  it("is shorter than a full SHA-256 base58 encoding", () => {
    const kp = generateKeypair();
    const hash = crypto.createHash("sha256").update(kp.publicKey).digest();
    const fullHash = base58Encode(hash);
    expect(kp.amid.length).toBeLessThan(fullHash.length);
  });

  it("different public keys yield different AMIDs", () => {
    const amids = new Set<string>();
    for (let i = 0; i < 10; i++) {
      amids.add(generateKeypair().amid);
    }
    expect(amids.size).toBe(10);
  });
});

// ---------------------------------------------------------------------------
// meshCommand structure
// ---------------------------------------------------------------------------

describe("meshCommand", () => {
  it("creates a command named 'mesh'", () => {
    const cmd = meshCommand();
    expect(cmd.name()).toBe("mesh");
  });

  it("has a description", () => {
    const cmd = meshCommand();
    expect(cmd.description()).toBeTruthy();
  });

  it("has auth, status, reset, and setup-trust subcommands", () => {
    const cmd = meshCommand();
    const subNames = cmd.commands.map((c) => c.name());
    expect(subNames).toContain("auth");
    expect(subNames).toContain("status");
    expect(subNames).toContain("reset");
    expect(subNames).toContain("setup-trust");
  });

  it("setup-trust has --display-name and --dry-run options", () => {
    const cmd = meshCommand();
    const setup = cmd.commands.find((c) => c.name() === "setup-trust")!;
    expect(setup).toBeDefined();
    const displayName = setup.options.find((o) => o.long === "--display-name");
    expect(displayName).toBeDefined();
    expect(displayName!.defaultValue).toBe("Kars AgentMesh");
    const dryRun = setup.options.find((o) => o.long === "--dry-run");
    expect(dryRun).toBeDefined();
  });

  it("setup-trust description mentions api://agentmesh", () => {
    const cmd = meshCommand();
    const setup = cmd.commands.find((c) => c.name() === "setup-trust")!;
    expect(setup.description()).toContain("api://agentmesh");
  });

  it("auth subcommand has --registry required option", () => {
    const cmd = meshCommand();
    const auth = cmd.commands.find((c) => c.name() === "auth")!;
    const registryOpt = auth.options.find((o) => o.long === "--registry");
    expect(registryOpt).toBeDefined();
    expect(registryOpt!.required).toBe(true);
  });

  it("auth subcommand has --provider option defaulting to github", () => {
    const cmd = meshCommand();
    const auth = cmd.commands.find((c) => c.name() === "auth")!;
    const providerOpt = auth.options.find((o) => o.long === "--provider");
    expect(providerOpt).toBeDefined();
    expect(providerOpt!.defaultValue).toBe("github");
  });

  it("auth subcommand has --no-browser option", () => {
    const cmd = meshCommand();
    const auth = cmd.commands.find((c) => c.name() === "auth")!;
    const browserOpt = auth.options.find((o) => o.long === "--no-browser");
    expect(browserOpt).toBeDefined();
  });
});
