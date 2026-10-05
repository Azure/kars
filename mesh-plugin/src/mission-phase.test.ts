// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { filesystemPhase, missionPhaseDigest, validPhaseEvidence } from "./mission-phase.js";

const input = { name: "write-briefing", objective: "Write a useful internal briefing.", maxToolCalls: 2 };
const phase = filesystemPhase({ ...input, capabilities: ["filesystem-write"], minToolCalls: 1 });
const evidence = { name: phase.name, attemptedToolCalls: 1, successfulToolCalls: 1, minToolCalls: 1, maxToolCalls: 2 };

describe("shared filesystem phase contract", () => {
  it("canonicalizes defaults, key order and capability order without mutating input", () => {
    expect(filesystemPhase(input)).toEqual({ ...input, capabilities: [], requiredToolCalls: [], minToolCalls: 0, freshContext: false });
    expect(missionPhaseDigest(input)).toBe(missionPhaseDigest(filesystemPhase(input)));
    const capabilities = ["filesystem-write", "filesystem-read"];
    const result = filesystemPhase({ ...input, capabilities });
    expect(result.capabilities).toEqual(["filesystem-read", "filesystem-write"]);
    expect(capabilities).toEqual(["filesystem-write", "filesystem-read"]);
    expect(missionPhaseDigest({ capabilities, ...input })).toBe(missionPhaseDigest({ ...input, capabilities: [...capabilities].reverse() }));
    capabilities.length = 0;
    expect(result.capabilities).toHaveLength(2);
    expect(missionPhaseDigest(Object.assign(Object.create(null), input))).toBe(missionPhaseDigest(input));
  });

  it.each([
    { objective: "Write a different internal briefing." }, { name: "revised" },
    { maxToolCalls: 1 }, { capabilities: ["filesystem-read"] }, { freshContext: true },
  ])("binds changes to the digest: %j", patch => {
    expect(missionPhaseDigest({ ...input, ...patch })).not.toBe(missionPhaseDigest(input));
  });

  it.each([
    { capabilities: ["shell"] }, { capabilities: ["filesystem-read", "filesystem-read"] },
    { capabilities: null }, { requiredToolCalls: [{}] }, { requiredToolCalls: null },
    { minToolCalls: 1 }, { maxToolCalls: 33 }, { maxToolCalls: -1 }, { maxToolCalls: 1.5 },
    { minToolCalls: null }, { freshContext: null }, { extra: true }, { objective: "too short" },
  ])("rejects unsupported or malformed contracts: %j", patch => {
    expect(() => filesystemPhase({ ...input, ...patch })).toThrow();
  });

  it("rejects hidden, symbol and accessor fields without invoking accessors", () => {
    let reads = 0;
    for (const value of [
      Object.defineProperty({ ...input }, "hidden", { value: true }),
      { ...input, [Symbol("hidden")]: true },
      Object.defineProperty({ ...input }, "objective", { enumerable: true, get: () => { reads++; return input.objective; } }),
    ]) expect(() => filesystemPhase(value)).toThrow();
    expect(reads).toBe(0);
  });

  it("rejects sparse, decorated, accessor and subclass arrays", () => {
    let reads = 0;
    class Capabilities extends Array<string> {}
    for (const capabilities of [
      Object.assign([], { length: 1 }), Object.assign(["filesystem-read"], { extra: true }),
      Object.defineProperty([], "hidden", { value: true }),
      Object.assign([], { [Symbol("hidden")]: true }),
      Object.defineProperty(["filesystem-read"], "0", { get: () => { reads++; return "filesystem-read"; } }),
      new Capabilities("filesystem-read"),
    ]) expect(() => filesystemPhase({ ...input, capabilities })).toThrow();
    expect(reads).toBe(0);
  });

  it("validates exact bounded evidence and the successful minimum", () => {
    expect(validPhaseEvidence(evidence, phase, true)).toBe(true);
    expect(validPhaseEvidence({ ...evidence, attemptedToolCalls: 0, successfulToolCalls: 0 }, phase, false)).toBe(true);
    expect(validPhaseEvidence({ ...evidence, successfulToolCalls: 0 }, phase, true)).toBe(false);
    expect(validPhaseEvidence(Object.assign(Object.create(null), evidence), phase, true)).toBe(true);
  });

  it.each([
    { name: "other" }, { minToolCalls: 0 }, { maxToolCalls: 3 }, { attemptedToolCalls: 3 },
    { attemptedToolCalls: -1 }, { successfulToolCalls: 2 }, { successfulToolCalls: 0.5 },
    { attemptedToolCalls: NaN }, { attemptedToolCalls: Infinity }, { successfulToolCalls: undefined }, { extra: true },
  ])("rejects invalid evidence: %j", patch => {
    expect(validPhaseEvidence({ ...evidence, ...patch }, phase, false)).toBe(false);
  });

  it("fails closed on reflective exceptions and accessor evidence", () => {
    const revoked = Proxy.revocable({}, {}); revoked.revoke();
    expect(() => filesystemPhase(revoked.proxy)).toThrow();
    expect(validPhaseEvidence(revoked.proxy, phase, false)).toBe(false);
    let reads = 0;
    expect(validPhaseEvidence(Object.defineProperty({ ...evidence }, "attemptedToolCalls", {
      enumerable: true, get: () => { reads++; return 1; },
    }), phase, true)).toBe(false);
    expect(reads).toBe(0);
  });
});
