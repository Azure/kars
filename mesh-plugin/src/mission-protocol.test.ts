// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { MAX_MISSION_ARTIFACT_BYTES, MAX_MISSION_MESSAGE_BYTES, parseMissionMessage, validMissionArtifacts } from "./mission-protocol.js";

const assignment = { type: "mission:assign", version: 1, taskName: "briefing", taskUid: "task-uid", sandboxUid: "sandbox-uid", podUid: "pod-uid", runNonce: "run-1", agentDid: `did:mesh:${"a".repeat(32)}`, dispatcherDid: `did:mesh:${"b".repeat(32)}`, bootId: "boot-1", assignmentId: "assignment-1", content: "Write a briefing" };
const reply = { ...assignment, type: "mission:reply", status: "succeeded", output: "Briefing attached", evidence: { model: "model", rounds: 1, usage: { promptTokens: 1, completionTokens: 2, totalTokens: 3 } } };

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
