// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

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

  it("generates an identity with AMID", async () => {
    const id = await generateIdentity();
    expect(id.amid).toBeTruthy();
    expect(id.amid.length).toBeGreaterThan(10);
    expect(id.signingPublicKey.length).toBe(32);
    expect(id.signingPrivateKey.length).toBe(32);
  });

  it("generates different AMIDs for different keys", async () => {
    const id1 = await generateIdentity();
    const id2 = await generateIdentity();
    expect(id1.amid).not.toBe(id2.amid);
  });

  it("AMID is deterministic for same public key", async () => {
    const id = await generateIdentity();
    const amid1 = deriveAmid(id.signingPublicKey);
    const amid2 = deriveAmid(id.signingPublicKey);
    expect(amid1).toBe(amid2);
  });

  it("AMID uses base58 alphabet", async () => {
    const id = await generateIdentity();
    const base58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    for (const c of id.amid) {
      expect(base58).toContain(c);
    }
  });

  it("saves and loads identity (roundtrip)", async () => {
    const id = await generateIdentity();
    await saveIdentity(id);

    const loaded = await loadIdentity();
    expect(loaded).not.toBeNull();
    expect(loaded!.amid).toBe(id.amid);
    expect(loaded!.signingPublicKey.toString("base64")).toBe(
      id.signingPublicKey.toString("base64")
    );
    expect(loaded!.signingPrivateKey.toString("base64")).toBe(
      id.signingPrivateKey.toString("base64")
    );
  });

  it("loadIdentity returns null when no file exists", async () => {
    const result = await loadIdentity();
    if (result) {
      expect(result.amid).toBeTruthy();
    }
  });

  it("loadOrCreateIdentity always returns an identity", async () => {
    const id = await loadOrCreateIdentity();
    expect(id.amid).toBeTruthy();
    expect(id.signingPublicKey.length).toBe(32);
  });
});
