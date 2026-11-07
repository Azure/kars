// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { nativeMissionLimits } from "./mission-phase.js";
import { missionAttemptName, missionContentDigest, type MissionAttempt, type MissionCandidate } from "./mission-dispatcher.js";
import { readRetainedMissionInputs } from "./mission-input-custody.js";
import { missionInputContentDigest, type MissionInputReference } from "./mission-inputs.js";
import { missionContract, type MissionAssignment, type MissionReply } from "./mission-protocol.js";
import { missionRecordOwner, missionResultMaps, packMissionAttempt, unpackMissionAttempt, type MissionConfigMap } from "./mission-record.js";

const content = "# Upstream draft\r\nCafé — 日本語 🌍\n";
function harness(version: 1 | 2 | 3 = 1) {
  const candidate: MissionCandidate = {
    namespace: "kars-system", taskName: "writer", taskUid: "writer-uid", sandboxUid: "writer-sandbox", podUid: "writer-pod",
    runNonce: "writer-run", agentDid: `did:mesh:${"a".repeat(32)}`, dispatcherDid: `did:mesh:${"b".repeat(32)}`,
    agentName: "Document writer", content: "Write a useful document with a named Markdown artifact.",
  };
  const reference: MissionInputReference = { name: "review-input.md", sha256: missionInputContentDigest(content), source: {
    namespace: candidate.namespace, taskName: candidate.taskName, taskUid: candidate.taskUid, sandboxUid: candidate.sandboxUid,
    podUid: candidate.podUid, runNonce: candidate.runNonce, agentDid: candidate.agentDid, assignmentId: "writer-assignment", artifactName: "draft.md",
  } };
  if (version !== 1) candidate.reviewedPhase = missionContract({ name: "prepare-draft", objective: "Read source material and prepare a useful draft",
    capabilities: ["filesystem-read", "filesystem-write"], minToolCalls: 1, maxToolCalls: 2 }).reviewedPhase;
  if (version === 3) candidate.inputArtifacts = [{ name: "source.md", source: { ...reference.source, taskUid: "earlier-uid" },
    content: "Earlier source", sha256: missionInputContentDigest("Earlier source") }];
  const contract = missionContract(candidate.reviewedPhase, candidate.inputArtifacts);
  const assignment: MissionAssignment = { ...contract, type: "mission:assign", taskName: candidate.taskName, taskUid: candidate.taskUid,
    sandboxUid: candidate.sandboxUid, podUid: candidate.podUid, runNonce: candidate.runNonce, agentDid: candidate.agentDid,
    dispatcherDid: candidate.dispatcherDid, bootId: "writer-boot", assignmentId: reference.source.assignmentId, artifactFormat: "text-v1",
    content: candidate.content, ...(candidate.inputArtifacts ? { inputArtifacts: candidate.inputArtifacts } : {}) };
  const evidence = { model: "gpt-5.4-mini", rounds: 2, usage: { promptTokens: 3, completionTokens: 2, totalTokens: 5 },
    ...(candidate.reviewedPhase ? { phase: { name: "prepare-draft", attemptedToolCalls: 1, successfulToolCalls: 1, minToolCalls: 1, maxToolCalls: 2 } } : {}) };
  const replyFields = { ...assignment };
  delete replyFields.inputArtifacts;
  const reply: MissionReply = { ...replyFields, type: "mission:reply", status: "succeeded",
    output: "Attached: draft.md", artifacts: { "draft.md": content, "notes.md": "Separate notes" }, evidence };
  const attempt: MissionAttempt = { version: 1, candidate, assignment, contentDigest: missionContentDigest(candidate.content), ownerSession: "dispatcher-session",
    startedAt: "2026-10-05T00:00:00.000Z", updatedAt: "2026-10-05T00:00:01.000Z", phase: "succeeded",
    events: [{ at: "2026-10-05T00:00:01.000Z", status: "succeeded", evidence }], reply };
  const record: MissionConfigMap = { apiVersion: "v1", kind: "ConfigMap", metadata: { namespace: candidate.namespace,
    name: missionAttemptName(candidate), uid: "record-uid", resourceVersion: "5", ownerReferences: [missionRecordOwner(candidate)] },
    data: { "attempt.json": JSON.stringify(attempt) } };
  const expected = missionResultMaps(attempt);
  const output = { ...expected.output, immutable: true, metadata: { ...expected.output.metadata, uid: "output-uid", resourceVersion: "6" } };
  const artifacts = { ...expected.artifacts, immutable: true, metadata: { ...expected.artifacts.metadata, uid: "artifacts-uid", resourceVersion: "7" } };
  const maps = new Map([record, output, artifacts].map(cm => [cm.metadata.name, cm]));
  const task = { apiVersion: "kars.azure.com/v1alpha1", kind: "KarsTask", metadata: { name: candidate.taskName, namespace: candidate.namespace,
    uid: candidate.taskUid, resourceVersion: "8", deletionTimestamp: undefined as string | undefined,
    annotations: { "kars.azure.com/requested-assignment-nonce": "a-later-run" } } };
  const reader = { task: vi.fn(async () => structuredClone(task)), configMap: vi.fn(async (_namespace: string, name: string) => structuredClone(maps.get(name))) };
  const saveAttempt = () => { record.data["attempt.json"] = JSON.stringify(attempt); };
  return { candidate, reference, attempt, record, output, artifacts, maps, task, reader, saveAttempt };
}

describe("shared durable mission records", () => {
  it.each([false, true, "full"])("retains native Activity and full input custody within transport and ConfigMap budgets (continuity=%s)", continuity => {
    const h = harness(3);
    const toolCount = continuity === "full" ? 64 : 128;
    const cycle = JSON.parse(readFileSync(new URL("../../inference-router/tests/fixtures/router-observation-tool-cycle.json", import.meta.url), "utf8"));
    const events = Array.from({ length: toolCount }, (_, i) => cycle.filter((e: any) => !e.reported_in_round).map((e: any) => ({
      ...e, round: i + 1, seq: i * 7 + e.seq, ...(e.call_id ? { call_id: `call-${i + 1}` } : {}),
    }))).flat();
    const rounds = Array.from({ length: toolCount }, (_, i) => i + 1);
    const routerObservations = JSON.stringify({ version: 1, source: "runtime-forwarded-router-observations",
      coverage: "native-tool-correlated-only", correlated_tools: toolCount, scope_id: cycle[0].scope_id, rounds, state: "observed",
      trace: { scope_id: cycle[0].scope_id, rounds, coverage: "native-tool-correlated-only", durable: false,
        missing_rounds: [], dropped_events: 0, truncated: false, events } });
    h.candidate.content = "Reviewed research instructions. ".repeat(256);
    h.candidate.reviewedPhase = { ...h.candidate.reviewedPhase!, executionMode: "native", maxToolCalls: 128 };
    h.candidate.inputArtifacts![0].content = "x".repeat(60 * 1024);
    h.candidate.inputArtifacts![0].sha256 = missionInputContentDigest(h.candidate.inputArtifacts![0].content);
    if (continuity) {
      const source = h.candidate.inputArtifacts![0].source;
      const memory = [29571,13205,19474,43332,17511,5728,4869].map((bytes,index) => ({
        name:`memory-0-document-${index}.md`,source:{...source,artifactName:`document-${index}.md`},
        content:"x".repeat(bytes),sha256:missionInputContentDigest("x".repeat(bytes)),
      }));
      memory[0].content += "x".repeat(143430 - Buffer.byteLength(JSON.stringify(memory)));
      memory[0].sha256 = missionInputContentDigest(memory[0].content);
      expect(Buffer.byteLength(JSON.stringify(memory))).toBe(143430);
      h.candidate.inputArtifacts = [...memory,...[12000,7200,1000].map((bytes,index)=>({
        name:`peer-${index}.md`,source:{...source,artifactName:`peer-${index}.md`},
        content:"y".repeat(bytes),sha256:missionInputContentDigest("y".repeat(bytes)),
      }))];
    }
    if (continuity === "full") {
      const source = h.candidate.inputArtifacts![0].source;
      h.candidate.inputArtifacts = Array.from({ length: 6 }, (_, i) => ({ name: `source-${i}.md`,
        source: { ...source, artifactName: `source-${i}.md` }, content: "x".repeat(58 * 1024),
        sha256: missionInputContentDigest("x".repeat(58 * 1024)) }));
      h.candidate.inputArtifacts[0].content += "x".repeat(363508 - Buffer.byteLength(JSON.stringify(h.candidate.inputArtifacts)));
      h.candidate.inputArtifacts[0].sha256 = missionInputContentDigest(h.candidate.inputArtifacts[0].content);
      expect(Buffer.byteLength(JSON.stringify(h.candidate.inputArtifacts))).toBe(363508);
    }
    const contract = missionContract(h.candidate.reviewedPhase, h.candidate.inputArtifacts);
    Object.assign(h.attempt.assignment, contract, { content: h.candidate.content, inputArtifacts:h.candidate.inputArtifacts,
      activityFormat: "router-observations-v1" });
    expect(Buffer.byteLength(JSON.stringify(h.attempt.assignment))).toBeLessThan((continuity === "full" ? 576 : 192) * 1024);
    h.attempt.contentDigest = missionContentDigest(h.candidate.content);
    const evidence = { model: "reviewed", rounds: toolCount + 1, usage: { promptTokens: 1548000, completionTokens: 38700, totalTokens: 1586700 },
      phase: { name: "prepare-draft", attemptedToolCalls: toolCount, successfulToolCalls: toolCount, minToolCalls: 1, maxToolCalls: 128 } };
    h.attempt.reply = { ...h.attempt.reply!, ...contract, evidence, status: "running", output: undefined, artifacts: undefined,
      activityFormat: "router-observations-v1", routerObservations };
    h.attempt.phase = "running";
    h.attempt.events = Array.from({ length: nativeMissionLimits(h.candidate.reviewedPhase)!.events - 1 }, () => ({
      at: h.attempt.updatedAt, status: "running", evidence }));
    h.saveAttempt();
    if (continuity === "full") h.record.data["attempt.json"] = packMissionAttempt(h.attempt);
    expect(unpackMissionAttempt(h.record, h.candidate).attempt.reply?.routerObservations).toBe(routerObservations);
    expect(JSON.stringify(h.attempt.events)).not.toContain("routerObservations");
    h.attempt.phase = "succeeded";
    Object.assign(h.attempt.reply, { status: "succeeded", routerObservations: undefined, output: "Report attached",
      artifacts: { "draft.md": "x".repeat((continuity ? 20 : 120) * 1024), "kars-router-observations.json": routerObservations } });
    h.attempt.events.push({ at: h.attempt.updatedAt, status: "succeeded", evidence });
    h.saveAttempt();
    if (continuity === "full") h.record.data["attempt.json"] = packMissionAttempt(h.attempt);
    const retained = unpackMissionAttempt(h.record, h.candidate).attempt;
    const maps = missionResultMaps(retained);
    expect(maps.artifacts.data["kars-router-observations.json"]).toBe(routerObservations);
    expect(Buffer.byteLength(h.record.data["attempt.json"])).toBeLessThan(900 * 1024);
    for (const cm of [h.record, maps.output, maps.artifacts]) {
      expect(Buffer.byteLength(JSON.stringify(cm))).toBeLessThan(1024 * 1024);
    }
    if (continuity) {
      h.attempt.reply!.artifacts!["draft.md"] = "x".repeat(120 * 1024);
      h.saveAttempt();
      expect(() => unpackMissionAttempt(h.record, h.candidate)).toThrow("storage bounds");
      h.record.data["attempt.json"] = packMissionAttempt(h.attempt);
      expect(unpackMissionAttempt(h.record, h.candidate).attempt).toEqual(JSON.parse(JSON.stringify(h.attempt)));
      expect(Buffer.byteLength(h.record.data["attempt.json"])).toBeLessThan(900 * 1024);
    }
  });
  it("round-trips referenced inputs and retains exact custody and canonical projections", async () => {
    const h = harness(3);
    const original = JSON.stringify(h.attempt);
    h.record.data["attempt.json"] = packMissionAttempt(h.attempt);
    expect(JSON.stringify(h.attempt)).toBe(original);
    const stored = JSON.parse(h.record.data["attempt.json"]);
    expect(stored.inputArtifactsFromAssignment).toBe(true);
    expect(stored.candidate).not.toHaveProperty("inputArtifacts");
    expect(stored.assignment.inputArtifacts).toEqual(h.candidate.inputArtifacts);
    const decoded = unpackMissionAttempt(h.record, h.candidate).attempt;
    expect(decoded).toEqual(JSON.parse(original));
    expect(missionResultMaps(decoded)).toEqual(missionResultMaps(h.attempt));
    expect(await readRetainedMissionInputs([h.reference], h.reader)).toEqual([
      { ...h.reference, content },
    ]);
  });
  it.each([false, null, "true"])("rejects unsupported input reference marker %s", marker => {
    const h = harness(3);
    const stored = JSON.parse(packMissionAttempt(h.attempt));
    stored.inputArtifactsFromAssignment = marker;
    h.record.data["attempt.json"] = JSON.stringify(stored);
    expect(() => unpackMissionAttempt(h.record, h.candidate)).toThrow("storage reference");
  });
  it("rejects ambiguous references, missing bundles and changed input digests", () => {
    const h = harness(3);
    for (const mutate of [
      (stored: any) => { stored.candidate.inputArtifacts = null; },
      (stored: any) => { stored.candidate.inputArtifacts = stored.assignment.inputArtifacts; },
      (stored: any) => { delete stored.assignment.inputArtifacts; },
      (stored: any) => { stored.assignment.inputArtifacts = null; },
      (stored: any) => { stored.assignment.inputArtifacts[0].content += "tampered"; },
    ]) {
      const stored = JSON.parse(packMissionAttempt(h.attempt));
      mutate(stored);
      h.record.data["attempt.json"] = JSON.stringify(stored);
      expect(() => unpackMissionAttempt(h.record, h.candidate)).toThrow();
    }
    h.attempt.assignment.inputArtifacts = [];
    expect(() => packMissionAttempt(h.attempt)).toThrow("inputs differ");
  });
  it.each([1, 2, 3] as const)("decodes v%i journals and builds canonical/current projections from one contract", version => {
    const h = harness(version);
    expect(unpackMissionAttempt(h.record, h.candidate)).toEqual({ revision: "5", attempt: JSON.parse(JSON.stringify(h.attempt)) });
    const canonical = missionResultMaps(h.attempt), current = missionResultMaps(h.attempt, "current");
    expect(canonical.output.data).toEqual(current.output.data);
    expect(canonical.artifacts.data).toEqual({ "response.md": "Attached: draft.md", "draft.md": content, "notes.md": "Separate notes" });
    expect(current.artifacts.metadata.name).toBe("kars-mission-artifacts-writer");
    expect(current.output.metadata.annotations?.["kars.azure.com/mission-evidence-role"]).toBe("current");
    expect(canonical.output.metadata.annotations?.["kars.azure.com/mission-evidence-role"]).toBe("canonical");
    expect(canonical.output.immutable).toBeUndefined();
  });

  it.each([1, 2, 3] as const)("round-trips only the latest v%i provisional snapshot and keeps terminal custody separate", version => {
    const h = harness(version);
    h.attempt.assignment.activityFormat = "router-observations-v1";
    const terminal: MissionReply = { ...h.attempt.reply!, activityFormat: "router-observations-v1" };
    const snapshot = (responses: number) => JSON.stringify({ version: 1,
      source: "runtime-forwarded-router-observations", coverage: "returned-responses-only",
      responses, rounds: [1], state: "unavailable", reason: "Router observations unavailable after execution (reset, expiry or read failure)" });
    h.attempt.phase = "running";
    h.attempt.events[0].status = "running";
    h.attempt.reply = { ...terminal, status: "running", output: undefined, artifacts: undefined, routerObservations: snapshot(1) };
    h.saveAttempt();
    expect(unpackMissionAttempt(h.record, h.candidate).attempt.reply?.routerObservations).toBe(snapshot(1));
    h.attempt.reply.routerObservations = snapshot(2);
    h.attempt.events.push({ ...h.attempt.events[0] });
    h.saveAttempt();
    const running = unpackMissionAttempt(h.record, h.candidate).attempt;
    expect(running.reply?.routerObservations).toBe(snapshot(2));
    expect(running.events).toHaveLength(2);
    expect(JSON.stringify(running.events)).not.toContain("routerObservations");
    expect(() => missionResultMaps(running)).toThrow("no valid terminal reply");
    delete h.attempt.assignment.activityFormat;
    h.saveAttempt();
    expect(() => unpackMissionAttempt(h.record, h.candidate)).toThrow("Malformed durable mission attempt");
    h.attempt.assignment.activityFormat = "router-observations-v1";
    delete h.attempt.reply.activityFormat;
    h.saveAttempt();
    expect(() => unpackMissionAttempt(h.record, h.candidate)).toThrow("Malformed durable mission attempt");
    h.attempt.reply = terminal;
    h.attempt.phase = "succeeded";
    h.attempt.events.push({ ...h.attempt.events[0], status: "succeeded" });
    h.saveAttempt();
    const completed = unpackMissionAttempt(h.record, h.candidate).attempt;
    expect(completed.reply).not.toHaveProperty("routerObservations");
    expect(JSON.stringify(missionResultMaps(completed))).not.toContain("routerObservations");
    h.attempt.reply.routerObservations = snapshot(2);
    h.saveAttempt();
    expect(() => unpackMissionAttempt(h.record, h.candidate)).toThrow("Malformed durable mission attempt");
    expect(() => missionResultMaps(h.attempt)).toThrow("no valid terminal reply");
  });

  it.each([1, 2, 3] as const)("retains v%i failure Activity without exposing failed deliverables", version => {
    const h = harness(version);
    const activity = JSON.stringify({ version: 1, source: "runtime-forwarded-router-observations",
      coverage: "returned-responses-only", responses: 1, rounds: [1], state: "unavailable", reason: "Trace expired" });
    h.attempt.assignment.activityFormat = "router-observations-v1";
    h.attempt.phase = "failed";
    h.attempt.reply = { ...h.attempt.reply!, status: "failed", output: undefined, artifacts: undefined,
      error: "Write failed", activityFormat: "router-observations-v1", routerObservations: activity };
    h.attempt.events[0].status = "failed";
    h.saveAttempt();
    const retained = unpackMissionAttempt(h.record, h.candidate).attempt;
    const maps = missionResultMaps(retained);
    expect(maps.output.data).toMatchObject({ status: "failed", output: "", error: "Write failed", artifactCount: "1" });
    expect(maps.artifacts.data).toEqual({ "kars-router-observations.json": activity });
    expect(missionResultMaps(retained, "current").artifacts.data).toEqual(maps.artifacts.data);
    expect(retained.reply).not.toHaveProperty("artifacts");
    expect(JSON.stringify(retained.events)).not.toContain("routerObservations");
  });

  it.each([1, 2, 3] as const)("retains v%i failed explanations as diagnostics, never artifacts", version => {
    const h = harness(version);
    h.attempt.phase = "failed";
    h.attempt.reply = { ...h.attempt.reply!, status: "failed", output: "Exact-host approval is needed.",
      artifacts: undefined, error: "Required exports missing" };
    h.attempt.events[0].status = "failed";
    h.saveAttempt();
    const retained = unpackMissionAttempt(h.record, h.candidate).attempt;
    for (const role of ["canonical", "current"] as const) {
      const maps = missionResultMaps(retained, role);
      expect(maps.output.data).toMatchObject({ status: "failed", output: h.attempt.reply.output,
        error: "Required exports missing", artifactCount: "0" });
      expect(maps.artifacts.data).toEqual({});
      expect(JSON.parse(maps.output.data["evidence.json"]).output).toBe(h.attempt.reply.output);
    }
  });

  it("keeps failed terminal projections empty and rejects mismatched journals", () => {
    const h = harness();
    h.attempt.phase = "failed";
    h.attempt.reply = { ...h.attempt.reply!, status: "failed", output: undefined, artifacts: undefined, error: "Model failed" };
    h.attempt.events[0].status = "failed";
    h.saveAttempt();
    expect(unpackMissionAttempt(h.record, h.candidate).attempt.phase).toBe("failed");
    expect(missionResultMaps(h.attempt).artifacts.data).toEqual({});
    expect(missionResultMaps(h.attempt).output.data).toMatchObject({ status: "failed", output: "", artifactCount: "0" });
    h.attempt.events[0].status = "succeeded"; h.saveAttempt();
    expect(() => unpackMissionAttempt(h.record, h.candidate)).toThrow();
  });
});

describe("retained upstream input custody", () => {
  it.each([1, 2, 3] as const)("resolves exact v%i bytes from retained immutable results, independent of the current run", async version => {
    const h = harness(version);
    const result = await readRetainedMissionInputs([h.reference], h.reader);
    expect(result).toEqual([{ ...h.reference, content }]);
    expect([result, result[0], result[0].source].every(Object.isFrozen)).toBe(true);
    expect(h.reader.task).toHaveBeenCalledTimes(2);
    expect(h.reader.configMap).toHaveBeenCalledTimes(6);
    expect(h.reader.configMap.mock.calls.map(([, name]) => name)).toEqual([
      h.record.metadata.name, h.output.metadata.name, h.artifacts.metadata.name,
      h.artifacts.metadata.name, h.output.metadata.name, h.record.metadata.name,
    ]);
  });

  it("captures references before awaits, deduplicates reads, and never fetches current projections", async () => {
    const h = harness();
    const second = { ...h.reference, name: "notes.md", sha256: missionInputContentDigest("Separate notes"), source: { ...h.reference.source, artifactName: "notes.md" } };
    h.reader.task.mockImplementationOnce(async () => {
      h.reference.source.assignmentId = "caller-mutated"; h.reference.sha256 = missionInputContentDigest("changed");
      return structuredClone(h.task);
    });
    const expectedReference = structuredClone(h.reference);
    const result = await readRetainedMissionInputs([h.reference, second], h.reader);
    expect(result.find(item => item.name === "review-input.md")).toEqual({ ...expectedReference, content });
    expect(result[0].content).toBe("Separate notes");
    expect(h.reader.task).toHaveBeenCalledTimes(2);
    expect(h.reader.configMap).toHaveBeenCalledTimes(6);
  });

  it.each(["response.md", "kars-router-observations.json"])("rejects %s before any API read", async artifactName => {
    const h = harness(); h.reference.source.artifactName = artifactName;
    await expect(readRetainedMissionInputs([h.reference], h.reader)).rejects.toThrow("named useful document");
    expect(h.reader.task).not.toHaveBeenCalled(); expect(h.reader.configMap).not.toHaveBeenCalled();
  });

  it.each(["taskUid", "sandboxUid", "podUid", "runNonce", "assignmentId", "agentDid", "artifactName"] as const)("rejects a mismatched %s source pin", async key => {
    const h = harness();
    h.reference.source[key] = key === "agentDid" ? `did:mesh:${"c".repeat(32)}` : "different";
    await expect(readRetainedMissionInputs([h.reference], h.reader)).rejects.toThrow();
  });

  it.each(["mutable", "owner", "annotation", "label", "data", "namespace", "uid", "revision", "deleting"])("rejects %s canonical custody", async defect => {
    const h = harness();
    switch (defect) {
      case "mutable": h.artifacts.immutable = false; break;
      case "owner": h.artifacts.metadata.ownerReferences![0].uid = "wrong"; break;
      case "annotation": h.artifacts.metadata.annotations!["kars.azure.com/mission-run-nonce"] = "wrong"; break;
      case "label": h.artifacts.metadata.labels = {}; break;
      case "data": h.artifacts.data["draft.md"] = "Tampered"; break;
      case "namespace": h.artifacts.metadata.namespace = "elsewhere"; break;
      case "uid": h.artifacts.metadata.uid = ""; break;
      case "revision": h.artifacts.metadata.resourceVersion = ""; break;
      case "deleting": h.artifacts.metadata.deletionTimestamp = "2026-10-05T00:01:00Z"; break;
    }
    await expect(readRetainedMissionInputs([h.reference], h.reader)).rejects.toThrow();
  });

  it("requires both canonical maps, exact output evidence, named terminal attachment and matching content hash", async () => {
    for (const alter of [
      (h: ReturnType<typeof harness>) => { h.maps.delete(h.output.metadata.name); },
      (h: ReturnType<typeof harness>) => { h.maps.delete(h.artifacts.metadata.name); },
      (h: ReturnType<typeof harness>) => { h.output.data.assignmentId = "different"; },
      (h: ReturnType<typeof harness>) => { h.reference.sha256 = missionInputContentDigest("wrong"); },
      (h: ReturnType<typeof harness>) => { h.attempt.reply!.artifacts = {}; h.saveAttempt(); },
      (h: ReturnType<typeof harness>) => { h.attempt.reply!.status = "running"; h.saveAttempt(); },
      (h: ReturnType<typeof harness>) => { h.task.metadata.deletionTimestamp = "2026-10-05T00:01:00Z"; },
    ]) {
      const h = harness(); alter(h);
      await expect(readRetainedMissionInputs([h.reference], h.reader)).rejects.toThrow();
    }
  });

  it.each(["task", "record", "output", "artifacts"] as const)("rejects %s replacement during selection", async changed => {
    const h = harness();
    const seen = new Set<string>();
    h.reader.configMap.mockImplementation(async (_namespace, name) => {
      const cm = structuredClone(h.maps.get(name));
      if (cm && seen.has(name) && name === h[changed === "task" ? "record" : changed].metadata.name && changed !== "task") {
        cm.metadata.uid = "recreated";
      }
      seen.add(name); return cm;
    });
    if (changed === "task") h.reader.task.mockImplementationOnce(async () => structuredClone(h.task))
      .mockImplementationOnce(async () => ({ ...h.task, metadata: { ...h.task.metadata, resourceVersion: "99" } }));
    await expect(readRetainedMissionInputs([h.reference], h.reader)).rejects.toThrow("changed during input selection");
  });

  it("does not swallow authorization or API errors", async () => {
    const h = harness(); h.reader.configMap.mockRejectedValueOnce(new Error("403 Forbidden"));
    await expect(readRetainedMissionInputs([h.reference], h.reader)).rejects.toThrow("403 Forbidden");
  });
});
