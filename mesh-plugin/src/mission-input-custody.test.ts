// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { missionAttemptName, missionContentDigest, type MissionAttempt, type MissionCandidate } from "./mission-dispatcher.js";
import { readRetainedMissionInputs } from "./mission-input-custody.js";
import { missionInputContentDigest, type MissionInputReference } from "./mission-inputs.js";
import { missionContract, type MissionAssignment, type MissionReply } from "./mission-protocol.js";
import { missionRecordOwner, missionResultMaps, unpackMissionAttempt, type MissionConfigMap } from "./mission-record.js";

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
