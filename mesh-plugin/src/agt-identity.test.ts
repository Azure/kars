// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { AgtMeshIdentity, deriveAmid, deriveDid } from "./agt-identity.js";

function tempFile(): string {
  return path.join(
    fs.mkdtempSync(path.join(os.tmpdir(), "agt-identity-")),
    "identity.json",
  );
}

describe("AgtMeshIdentity", () => {
  it("creates a new identity if file does not exist and persists it", async () => {
    const file = tempFile();
    const id = await AgtMeshIdentity.loadOrCreate(file);
    expect(id.agentId).toMatch(/^did:agentmesh:/);
    expect(id.signingPublicKey).toHaveLength(32);
    expect(id.signingPrivateKey).toHaveLength(32);
    expect(fs.existsSync(file)).toBe(true);
    const stat = fs.statSync(file);
    expect((stat.mode & 0o777).toString(8)).toBe("600");
  });

  it("round-trips: second load yields the same agentId and keys", async () => {
    const file = tempFile();
    const a = await AgtMeshIdentity.loadOrCreate(file);
    const b = await AgtMeshIdentity.loadOrCreate(file);
    expect(b.agentId).toBe(a.agentId);
    expect(Buffer.from(b.signingPublicKey).toString("hex")).toBe(
      Buffer.from(a.signingPublicKey).toString("hex"),
    );
    expect(Buffer.from(b.signingPrivateKey).toString("hex")).toBe(
      Buffer.from(a.signingPrivateKey).toString("hex"),
    );
  });

  it("deriveAmid is stable for the same public key", () => {
    const pub = new Uint8Array(32).fill(7);
    expect(deriveAmid(pub)).toBe(deriveAmid(pub));
    expect(deriveDid(pub)).toBe(`did:agentmesh:${deriveAmid(pub)}`);
  });

  it("rejects a tampered ciphertext on reload", async () => {
    const file = tempFile();
    await AgtMeshIdentity.loadOrCreate(file);
    const raw = JSON.parse(fs.readFileSync(file, "utf8"));
    raw.encPriv = Buffer.from("00".repeat(32), "hex").toString("base64");
    fs.writeFileSync(file, JSON.stringify(raw));
    let threw = false;
    try {
      await AgtMeshIdentity.loadOrCreate(file);
    } catch {
      threw = true;
    }
    expect(threw).toBe(true);
  });
});
