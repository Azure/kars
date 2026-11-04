// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { filesystemPhase, missionPhaseDigest, nativeMissionLimits, reviewedFetchArguments, validPhaseEvidence } from "./mission-phase.js";
import { missionEvidenceAdvances } from "./mission-protocol.js";

const input = { name: "write-briefing", objective: "Write a useful internal briefing.", maxToolCalls: 2 };
const phase = filesystemPhase({ ...input, capabilities: ["filesystem-write"], minToolCalls: 1 });
const evidence = { name: phase.name, attemptedToolCalls: 1, successfulToolCalls: 1, minToolCalls: 1, maxToolCalls: 2 };

describe("reviewed exact public fetch contract", () => {
  it("matches the controller's complete required-fetch grant projection", () => {
    const contract = JSON.parse(readFileSync(new URL("../../controller/src/reviewed_execution/phase-projection-fixture.json", import.meta.url), "utf8"));
    expect(filesystemPhase(contract.reviewedPhase)).toEqual(contract.reviewedPhase);
    expect(missionPhaseDigest(contract.reviewedPhase)).toBe(contract.phaseDigest);
    expect(() => filesystemPhase({ ...contract.reviewedPhase, requiredToolCalls: [] })).toThrow();
  });
  const call = { name: "http_fetch", arguments: { method: "GET", url: "https://www.example.com/pricing?q=public%20plans" } };
  const input = { name: "research", objective: "Read the reviewed official public evidence.", capabilities: ["network"],
    requiredToolCalls: [call], minToolCalls: 0, maxToolCalls: 3, freshContext: true };
  it("preserves exact required authority, argument order and defensive copies", () => {
    const phase = filesystemPhase(input);
    expect(phase.requiredToolCalls).toEqual([call]);
    expect(phase.requiredToolCalls[0].arguments).not.toBe(call.arguments);
    expect(missionPhaseDigest({ ...input, requiredToolCalls: [{ arguments: { url: call.arguments.url, method: "GET" }, name: "http_fetch" }] }))
      .toBe(missionPhaseDigest(input));
    expect(missionPhaseDigest({ ...input, requiredToolCalls: [{ ...call, arguments: { ...call.arguments, url: "https://www.example.com/other" } }] }))
      .not.toBe(missionPhaseDigest(input));
    expect(reviewedFetchArguments(phase, call.arguments)).toBe(true);
    for (const args of [{ url: call.arguments.url }, { ...call.arguments, headers: {} }, { ...call.arguments, method: "POST" },
      { ...call.arguments, url: "https://www.example.com/unreviewed" }]) expect(reviewedFetchArguments(phase, args)).toBe(false);
  });
  it.each(["http://example.com/", "https://example.com", "https://EXAMPLE.com/", "https://example.com:443/",
    "https://user@example.com/", "https://127.0.0.1/", "https://[::1]/", "https://x.local/", "https://x.internal/",
    "https://example.com/#part", "https://example.com/a/../b", "https://example.com/%2e/b", "https://example.com/%no",
    "https://example.com/white space", "https://example.com/é"])("rejects noncanonical URL %s", url => {
    expect(() => filesystemPhase({ ...input, requiredToolCalls: [{ ...call, arguments: { method: "GET", url } }] })).toThrow();
  });
  it.each([{ capabilities: [] }, { requiredToolCalls: [] }, { requiredToolCalls: [call, call] }, { maxToolCalls: 0 },
    { requiredToolCalls: [{ ...call, arguments: { ...call.arguments, body: "" } }] },
    { requiredToolCalls: [{ name: "web_search", arguments: { query: "pricing" } }] }])("rejects incompatible authority %j", patch => {
    expect(() => filesystemPhase({ ...input, ...patch })).toThrow();
  });
  it("requires verified required-call credit independently of filesystem successes", () => {
    const phase = filesystemPhase(input);
    const e = { name: phase.name, minToolCalls: 0, maxToolCalls: 3, attemptedToolCalls: 2, successfulToolCalls: 2 };
    expect(validPhaseEvidence(e, phase, true)).toBe(false);
    expect(validPhaseEvidence({ ...e, completedRequiredToolCalls: 0 }, phase, false)).toBe(true);
    expect(validPhaseEvidence({ ...e, completedRequiredToolCalls: 1 }, phase, true)).toBe(true);
    for (const count of [null, -1, 0.5, 2, "1"]) expect(validPhaseEvidence({ ...e, completedRequiredToolCalls: count }, phase, true)).toBe(false);
    expect(validPhaseEvidence({ ...e, completedRequiredToolCalls: 1, successfulToolCalls: 0 }, phase, false)).toBe(false);
    const previous = { model: "model", rounds: 1, usage: null, phase: { ...e, completedRequiredToolCalls: 1 } };
    expect(missionEvidenceAdvances(previous, { ...previous, rounds: 2, phase: { ...e, completedRequiredToolCalls: 0 } })).toBe(false);
    expect(missionEvidenceAdvances(previous, { ...previous, rounds: 2, phase: e })).toBe(false);
  });
});

describe("native phase authority", () => {
  const native = { name: "research", objective: "Research the task using the approved public sources.",
    capabilities: ["filesystem-write", "network"], requiredToolCalls: [], minToolCalls: 0,
    maxToolCalls: 8, freshContext: true, executionMode: "native" };

  it("permits agent-selected GETs without prescribing a source or tool sequence", () => {
    const phase = filesystemPhase(native);
    expect(phase.executionMode).toBe("native");
    expect(phase.requiredToolCalls).toEqual([]);
    expect(reviewedFetchArguments(phase, { method: "GET", url: "https://example.org/agent-selected" })).toBe(true);
    expect(validPhaseEvidence({ name: phase.name, minToolCalls: 0, maxToolCalls: 8,
      attemptedToolCalls: 0, successfulToolCalls: 0 }, phase, true)).toBe(true);
    expect(reviewedFetchArguments(filesystemPhase({ ...native, capabilities: ["filesystem-write"] }),
      { method: "GET", url: "https://example.org/agent-selected" })).toBe(false);
  });

  it("accepts a native origin URL without changing historical exact-URL grants", () => {
    const phase = filesystemPhase(native);
    expect(reviewedFetchArguments(phase, { method: "GET", url: "https://example.net" })).toBe(true);
    const legacy = filesystemPhase({ name: native.name, objective: native.objective, maxToolCalls: 8,
      capabilities: ["network"], requiredToolCalls: [{ name: "http_fetch", arguments: { method: "GET", url: "https://example.net/" } }] });
    expect(reviewedFetchArguments(legacy, { method: "GET", url: "https://example.net" })).toBe(false);
    for (const url of ["https://127.0.0.1", "https://example.local", "https://user@example.net", "https://example.net#fragment", "https://example.net?x=1"])
      expect(reviewedFetchArguments(phase, { method: "GET", url })).toBe(false);
  });

  it("binds explicit native memory without granting network or widening legacy authority", () => {
    const phase = filesystemPhase({ ...native, capabilities: ["memory"] });
    expect(phase.capabilities).toEqual(["memory"]);
    expect(reviewedFetchArguments(phase, { method: "GET", url: "https://example.org/" })).toBe(false);
    const all = ["network", "memory", "filesystem-write", "filesystem-read", "github-pr", "python-execute"];
    for (const missing of ["filesystem-read", "filesystem-write", "network"])
      expect(() => filesystemPhase({ ...native, capabilities: all.filter(c => c !== missing) })).toThrow();
    expect(filesystemPhase({ ...native, capabilities: all }).capabilities).toEqual([...all].sort());
    expect(missionPhaseDigest({ ...native, capabilities: all }))
      .toBe(missionPhaseDigest({ ...native, capabilities: [...all].reverse() }));
    expect(missionPhaseDigest(phase)).not.toBe(missionPhaseDigest({ ...native, capabilities: [] }));
    const { executionMode: _mode, ...legacy } = phase;
    expect(() => filesystemPhase(legacy)).toThrow();
    expect(() => filesystemPhase({ ...native, capabilities: [...all, "shell"] })).toThrow();
  });

  it("derives native round, wall-clock and progress headroom from the reviewed phase", () => {
    expect(nativeMissionLimits(undefined)).toBeUndefined();
    expect(nativeMissionLimits(filesystemPhase(input))).toBeUndefined();
    expect(nativeMissionLimits(filesystemPhase({ ...native, maxToolCalls: 128 })))
      .toEqual({ rounds: 130, timeoutMs: 120_300_000, events: 646 });
    expect(nativeMissionLimits(filesystemPhase({ ...native, maxToolCalls: 2_147_483_647 }))?.timeoutMs).toBe(2_147_483_647);
  });

  it.each([33, 128, 2_147_483_647])("honors reviewed native headroom %i without widening legacy phases", maxToolCalls => {
    const phase = filesystemPhase({ ...native, minToolCalls: maxToolCalls, maxToolCalls });
    expect(phase.maxToolCalls).toBe(maxToolCalls);
    const evidence = { name: phase.name, minToolCalls: maxToolCalls, maxToolCalls,
      attemptedToolCalls: maxToolCalls, successfulToolCalls: maxToolCalls };
    expect(validPhaseEvidence(evidence, phase, true)).toBe(true);
    expect(validPhaseEvidence({ ...evidence, attemptedToolCalls: maxToolCalls + 1 }, phase, true)).toBe(false);
    const { executionMode: _mode, ...legacy } = phase;
    expect(() => filesystemPhase(legacy)).toThrow();
  });

  it("binds native authority without changing historical canonical bytes", () => {
    const legacy = JSON.parse(readFileSync(new URL("../../controller/src/reviewed_execution/phase-projection-fixture.json", import.meta.url), "utf8"));
    expect(missionPhaseDigest(legacy.reviewedPhase)).toBe(legacy.phaseDigest);
    const phase = filesystemPhase({ ...legacy.reviewedPhase, executionMode: "native" });
    expect(Object.keys(phase).at(-1)).toBe("executionMode");
    expect(missionPhaseDigest(phase)).not.toBe(legacy.phaseDigest);
    const { executionMode: _mode, ...historical } = native;
    expect(() => filesystemPhase(historical)).toThrow();
  });

  it("retains explicit completion obligations without restricting additional sources", () => {
    const call = { name: "http_fetch", arguments: { method: "GET", url: "https://example.org/required" } };
    const phase = filesystemPhase({ ...native, requiredToolCalls: [call] });
    expect(reviewedFetchArguments(phase, { method: "GET", url: "https://example.org/another" })).toBe(true);
    const evidence = { name: phase.name, minToolCalls: 0, maxToolCalls: 8,
      attemptedToolCalls: 1, successfulToolCalls: 1, completedRequiredToolCalls: 0 };
    expect(validPhaseEvidence(evidence, phase, true)).toBe(false);
    expect(validPhaseEvidence({ ...evidence, completedRequiredToolCalls: 1 }, phase, true)).toBe(true);
  });

  it.each([{ executionMode: null }, { executionMode: "future" }, { capabilities: ["shell"] },
    { maxToolCalls: 2_147_483_648 }, { maxToolCalls: -1 }, { maxToolCalls: 1.5 },
    { minToolCalls: 9 }])("rejects malformed authority %j", patch => {
    expect(() => filesystemPhase({ ...native, ...patch })).toThrow();
  });
});

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
