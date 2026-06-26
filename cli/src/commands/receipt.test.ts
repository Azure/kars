// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import { generateKeyPairSync, sign as cryptoSign, createHash } from "node:crypto";
import { __test } from "./receipt.js";

const { pae, verifyReceipt } = __test;

const DSSE_PAYLOAD_TYPE = "application/vnd.in-toto+json";

/** Build a self-signed receipt + matching anchor, mirroring the controller. */
function makeSignedReceipt(overrides?: { tamperPayload?: boolean; wrongKey?: boolean }) {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const rawPub = publicKey.export({ type: "spki", format: "der" }).subarray(-32);
  const keyId = createHash("sha256").update(rawPub).digest("hex");

  const envelopeDigest = "sha256:deadbeefdeadbeefdeadbeefdeadbeef";
  const statement = {
    _type: "https://in-toto.io/Statement/v1",
    subject: [
      {
        name: "kars-system/demo",
        digest: { sha256: "deadbeefdeadbeefdeadbeefdeadbeef" },
      },
    ],
    predicateType: "https://kars.azure.com/attestations/GovernanceReceipt/v0",
    predicate: {
      claims: [
        { class: "integrity", status: "PASS", detail: "signed" },
        { class: "completeness", status: "PARTIAL", detail: "governance only" },
      ],
    },
  };
  const payloadBody = Buffer.from(JSON.stringify(statement), "utf8");
  const message = pae(DSSE_PAYLOAD_TYPE, payloadBody);
  const signature = cryptoSign(null, message, privateKey);

  const payloadB64 = overrides?.tamperPayload
    ? Buffer.from(JSON.stringify({ ...statement, subject: [{ name: "evil" }] }), "utf8").toString("base64")
    : payloadBody.toString("base64");

  const receipt = {
    metadata: { name: "demo", namespace: "kars-system" },
    spec: {
      taskRef: { name: "demo" },
      envelopeDigest,
      predicateType: statement.predicateType,
      scheme: "DSSEv1+ed25519",
      keyId,
      dsse: {
        payload: payloadB64,
        payloadType: DSSE_PAYLOAD_TYPE,
        signatures: [{ keyid: keyId, sig: signature.toString("base64") }],
      },
      claims: statement.predicate.claims,
    },
  };

  const anchorKeyId = overrides?.wrongKey
    ? createHash("sha256").update(Buffer.alloc(32, 7)).digest("hex")
    : keyId;
  const anchorPub = overrides?.wrongKey
    ? generateKeyPairSync("ed25519").publicKey.export({ type: "spki", format: "der" }).subarray(-32)
    : rawPub;

  const anchor = {
    keyId: anchorKeyId,
    publicKey: Buffer.from(anchorPub).toString("base64"),
    scheme: "DSSEv1+ed25519",
    payloadType: DSSE_PAYLOAD_TYPE,
  };

  return { receipt, anchor };
}

describe("receipt verify — PAE", () => {
  it("matches the DSSE framing byte-for-byte", () => {
    const got = pae("application/vnd.in-toto+json", Buffer.from("{}"));
    expect(got.toString("latin1")).toBe("DSSEv1 28 application/vnd.in-toto+json 2 {}");
  });
});

describe("receipt verify — verifyReceipt", () => {
  it("verifies a well-formed, correctly-signed receipt", () => {
    const { receipt, anchor } = makeSignedReceipt();
    const res = verifyReceipt(receipt, anchor);
    expect(res.ok).toBe(true);
    expect(res.checks.find((c) => c.name === "signature")?.ok).toBe(true);
    expect(res.checks.find((c) => c.name === "envelopeBinding")?.ok).toBe(true);
    expect(res.checks.find((c) => c.name === "keyBinding")?.ok).toBe(true);
    expect(res.claims).toHaveLength(2);
  });

  it("fails when the payload was tampered after signing", () => {
    const { receipt, anchor } = makeSignedReceipt({ tamperPayload: true });
    const res = verifyReceipt(receipt, anchor);
    expect(res.ok).toBe(false);
    expect(res.checks.find((c) => c.name === "signature")?.ok).toBe(false);
  });

  it("fails when signed by a key the anchor does not trust", () => {
    const { receipt, anchor } = makeSignedReceipt({ wrongKey: true });
    const res = verifyReceipt(receipt, anchor);
    expect(res.ok).toBe(false);
    // Either key binding or signature fails — both are unacceptable.
    const keyBinding = res.checks.find((c) => c.name === "keyBinding")?.ok;
    const sig = res.checks.find((c) => c.name === "signature")?.ok;
    expect(keyBinding && sig).toBeFalsy();
  });

  it("fails when the receipt carries no DSSE envelope", () => {
    const res = verifyReceipt(
      { metadata: { name: "x", namespace: "kars-system" }, spec: {} },
      { keyId: "k", publicKey: "", scheme: "DSSEv1+ed25519", payloadType: DSSE_PAYLOAD_TYPE },
    );
    expect(res.ok).toBe(false);
  });
});
