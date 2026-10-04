// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { MAX_MISSION_ARTIFACT_BYTES, MAX_MISSION_MESSAGE_BYTES, isMissionMessage, missionContract, missionEvidenceAdvances, parseMissionMessage, snapshotMissionData, validMissionArtifacts } from "./mission-protocol.js";

const assignment = { type: "mission:assign", version: 1, taskName: "briefing", taskUid: "task-uid", sandboxUid: "sandbox-uid", podUid: "pod-uid", runNonce: "run-1", agentDid: `did:mesh:${"a".repeat(32)}`, dispatcherDid: `did:mesh:${"b".repeat(32)}`, bootId: "boot-1", assignmentId: "assignment-1", content: "Write a briefing" };
const reply = { ...assignment, type: "mission:reply", status: "succeeded", output: "Briefing attached", evidence: { model: "model", rounds: 1, usage: { promptTokens: 1, completionTokens: 2, totalTokens: 3 } } };

describe("mandatory reviewed phase contract", () => {
  const phase = { name: "write-briefing", objective: "Write a bounded useful briefing", capabilities: ["filesystem-write", "filesystem-read"], minToolCalls: 1, maxToolCalls: 2 };
  const contract = missionContract(phase);
  const measured = { ...reply.evidence, phase: { name: phase.name, attemptedToolCalls: 1, successfulToolCalls: 1, minToolCalls: 1, maxToolCalls: 2 } };
  const success = { ...reply, ...contract, evidence: measured };
  it("canonicalizes, copies and freezes negotiated phase data without mutating the caller", () => {
    const source = structuredClone({ ...assignment, ...contract });
    source.reviewedPhase!.capabilities.reverse();
    const parsed = parseMissionMessage(source)!;
    expect(parsed).toMatchObject(contract);
    expect(source.reviewedPhase!.capabilities).toEqual(["filesystem-write", "filesystem-read"]);
    source.reviewedPhase!.maxToolCalls = 32;
    expect(parsed.reviewedPhase!.maxToolCalls).toBe(2);
    expect(Object.isFrozen(parsed)).toBe(true);
    expect(Object.isFrozen(parsed.reviewedPhase!.capabilities)).toBe(true);
    expect(parseMissionMessage(success)).not.toBeNull();
  });
  it.each([
    { version: 1, reviewedPhase: contract.reviewedPhase },
    { version: 1, phaseDigest: contract.phaseDigest },
    { version: 2 }, { version: 2, reviewedPhase: contract.reviewedPhase },
    { ...contract, phaseDigest: "sha256:" + "0".repeat(64) },
    { ...contract, reviewedPhase: { ...contract.reviewedPhase, maxToolCalls: 3 } },
  ])("rejects downgrade, omitted and changed phase contracts %#", fields => {
    expect(parseMissionMessage({ ...assignment, ...fields })).toBeNull();
  });
  it("does not allow phase evidence in legacy messages or execution evidence on probes/readiness", () => {
    expect(parseMissionMessage({ ...reply, evidence: measured })).toBeNull();
    for (const version of [missionContract(), contract]) {
      const probe = { ...assignment, ...version, type: "mission:probe", challenge: "probe" };
      expect(parseMissionMessage(probe)).not.toBeNull();
      expect(parseMissionMessage({ ...probe, evidence: measured })).toBeNull();
      expect(parseMissionMessage({ ...probe, type: "mission:reply", status: "ready", evidence: measured })).toBeNull();
    }
  });
  it.each([
    undefined, { ...measured, phase: undefined }, { ...measured, phase: { ...measured.phase, name: "different" } },
    { ...measured, phase: { ...measured.phase, attemptedToolCalls: 3 } },
    { ...measured, phase: { ...measured.phase, successfulToolCalls: 2 } },
    { ...measured, phase: { ...measured.phase, successfulToolCalls: 0 } },
    { ...measured, phase: { ...measured.phase, minToolCalls: 0 } },
    { ...measured, phase: { ...measured.phase, attemptedToolCalls: 1.5 } },
    { ...measured, model: "é".repeat(127) }, { ...measured, extra: "unbounded evidence" },
    { ...measured, usage: { ...measured.usage, extra: "unbounded usage" } },
  ])("rejects invalid successful accounting %#", evidence => {
    expect(parseMissionMessage({ ...success, evidence })).toBeNull();
  });
  it("permits bounded partial failure and running progress but not evidence-free running", () => {
    const evidence = { ...measured, phase: { ...measured.phase, successfulToolCalls: 0 } };
    expect(parseMissionMessage({ ...success, status: "running", evidence })).not.toBeNull();
    expect(parseMissionMessage({ ...success, status: "failed", evidence })).not.toBeNull();
    expect(parseMissionMessage({ ...success, status: "running", evidence: undefined })).toBeNull();
    expect(parseMissionMessage({ ...success, status: "failed", evidence: undefined })).not.toBeNull();
  });
  it("rejects erasure, counter, model, round and usage regressions", () => {
    expect(missionEvidenceAdvances(undefined, measured)).toBe(true);
    expect(missionEvidenceAdvances(measured, measured)).toBe(true);
    for (const next of [undefined, { ...measured, phase: undefined }, { ...measured, model: "different" },
      { ...measured, rounds: 0 }, { ...measured, phase: { ...measured.phase, attemptedToolCalls: 0 } },
      { ...measured, phase: { ...measured.phase, successfulToolCalls: 0 } },
      { ...measured, usage: { promptTokens: 0, completionTokens: 2, totalTokens: 2 } }]) {
      expect(missionEvidenceAdvances(measured, next)).toBe(false);
    }
    expect(missionEvidenceAdvances(measured, { ...measured, usage: null })).toBe(true);
    expect(missionEvidenceAdvances({ ...measured, usage: null }, measured)).toBe(false);
    expect(missionEvidenceAdvances({ ...measured, rounds: 0, usage: null }, measured)).toBe(true);
  });
  it("rejects nested accessors without reading them, consuming malformed reserved envelopes", () => {
    const get = vi.fn(() => "mission:assign");
    const accessor = Object.defineProperty({}, "type", { get, enumerable: true });
    expect(isMissionMessage(accessor)).toBe(true);
    expect(parseMissionMessage(accessor)).toBeNull();
    expect(parseMissionMessage({ ...success, evidence: Object.defineProperty({}, "model", { get, enumerable: true }) })).toBeNull();
    expect(get).not.toHaveBeenCalled();
    expect(parseMissionMessage(Object.create(assignment))).toBeNull();
  });
  it("bounds snapshots by depth, nodes, structure and serialized bytes", () => {
    let deep: unknown = "leaf";
    for (let i = 0; i < 18; i++) deep = { child: deep };
    for (const invalid of [deep, Array.from({ length: 16385 }, () => null), Object.assign([], { length: 1 }),
      Object.assign([], { extra: 1 }), { [Symbol("hidden")]: 1 }, Object.defineProperty({}, "hidden", { value: 1 }),
      { text: "é".repeat(MAX_MISSION_MESSAGE_BYTES) }]) expect(() => snapshotMissionData(invalid)).toThrow();
    expect(snapshotMissionData({ keep: 1, omit: undefined })).toEqual({ keep: 1 });
  });
  it("rechecks the wire bound after canonical defaults expand the phase", () => {
    const wire = { ...assignment, ...contract, reviewedPhase: phase, padding: "" };
    wire.padding = "x".repeat(MAX_MISSION_MESSAGE_BYTES - Buffer.byteLength(JSON.stringify(wire)));
    expect(Buffer.byteLength(JSON.stringify(wire))).toBe(MAX_MISSION_MESSAGE_BYTES);
    expect(parseMissionMessage(wire)).toBeNull();
  });
});

describe("bounded text mission artifacts", () => {
  it("preserves Unicode bytes in ordinary and null-prototype dictionaries", () => {
    const artifacts = { "briefing.md": "# Briefing\r\nCafé — 日本語 🌍\n" };
    expect(validMissionArtifacts(artifacts)).toBe(true);
    expect(validMissionArtifacts(Object.assign(Object.create(null), artifacts))).toBe(true);
    expect(parseMissionMessage({ ...reply, artifactFormat: "text-v1", artifacts })).toMatchObject({ artifacts });
  });
  it.each(["response.md", "__proto__", "constructor", "prototype", "../secret", "/tmp/file", ".hidden", "dir/file", "a\\b", "bad\nname", "a".repeat(129)])("rejects reserved or unsafe name %j", name => {
    expect(validMissionArtifacts(Object.fromEntries([[name, "text"]]))).toBe(false);
  });
  it.each([null, [], new Date(), new Map(), Object.create({ inherited: "text" }), { "a.md": " " }, { "a.md": 1 }, { "a.md": null }, { "a.md": 1n }])("rejects malformed dictionaries %#", value => {
    expect(validMissionArtifacts(value)).toBe(false);
  });
  it("rejects accessors, hidden keys, symbols and throwing proxies without reading content", () => {
    const get = vi.fn(() => { throw new Error("must not read"); });
    expect(validMissionArtifacts(Object.defineProperty({}, "a.md", { enumerable: true, get }))).toBe(false);
    expect(get).not.toHaveBeenCalled();
    expect(validMissionArtifacts(Object.defineProperty({}, "a.md", { value: "text" }))).toBe(false);
    expect(validMissionArtifacts({ [Symbol("file")]: "text" })).toBe(false);
    expect(validMissionArtifacts(new Proxy({}, { getPrototypeOf() { throw new Error("invalid"); } }))).toBe(false);
  });
  it("bounds the number of attached files", () => {
    const files = Object.fromEntries(Array.from({ length: 16 }, (_, i) => [`file-${i}.md`, "text"]));
    expect(validMissionArtifacts(files)).toBe(true);
    expect(validMissionArtifacts({ ...files, "extra.md": "text" })).toBe(false);
  });
  it("counts serialized UTF-8 bytes including escaping and map overhead", () => {
    const overhead = Buffer.byteLength(JSON.stringify({ "a.md": "" }));
    const exact = { "a.md": "x".repeat(MAX_MISSION_ARTIFACT_BYTES - overhead) };
    expect(validMissionArtifacts(exact)).toBe(true);
    expect(validMissionArtifacts({ "a.md": exact["a.md"] + "x" })).toBe(false);
    expect(validMissionArtifacts({ "a.md": "🌍".repeat(MAX_MISSION_ARTIFACT_BYTES / 4) })).toBe(false);
    expect(validMissionArtifacts({ "a.md": "\n".repeat(MAX_MISSION_ARTIFACT_BYTES / 2) })).toBe(false);
  });
  it("keeps old assignments and text-only replies compatible", () => {
    for (const message of [assignment, reply, { ...assignment, artifactFormat: "text-v1" }, { ...reply, artifactFormat: "text-v1" }]) expect(parseMissionMessage(message)).not.toBeNull();
    expect(parseMissionMessage({ ...assignment, artifactFormat: "text-v2" })).toBeNull();
    expect(parseMissionMessage({ ...reply, artifactFormat: "text-v2" })).toBeNull();
  });
  it.each(["ready", "accepted", "running", "failed", "rejected"])("rejects attachments on %s replies", status => {
    expect(parseMissionMessage({ ...reply, artifactFormat: "text-v1", artifacts: { "a.md": "text" }, status, challenge: "probe-1" })).toBeNull();
  });
  it("requires a negotiated success envelope and retains the whole-message bound", () => {
    const artifacts = { "a.md": "text" };
    expect(parseMissionMessage({ ...reply, artifacts })).toBeNull();
    expect(parseMissionMessage({ ...assignment, artifactFormat: "text-v1", artifacts })).toBeNull();
    expect(parseMissionMessage({ ...reply, artifactFormat: "text-v1", artifacts, output: "x".repeat(MAX_MISSION_MESSAGE_BYTES) })).toBeNull();
  });
});
