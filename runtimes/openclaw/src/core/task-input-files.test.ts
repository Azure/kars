// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { missionInputContentDigest } from "@kars/mesh/dist/mission-inputs.js";
import { readTaskFile, writeTaskFile, TaskInputFiles, TASK_INPUT_ROOT } from "./task-filesystem.js";

const source = { namespace: "kars-system", taskName: "writer", taskUid: "task-uid", sandboxUid: "sandbox-uid",
  podUid: "pod-uid", runNonce: "run-first", assignmentId: "assignment-first", agentDid: `did:mesh:${"a".repeat(32)}`, artifactName: "draft.md" };
function input(content = "Draft\r\n✓ 日本語 🌍", name = "draft.md") {
  return { name, content, source: { ...source }, sha256: missionInputContentDigest(content) };
}
const path = `${TASK_INPUT_ROOT}/draft.md`;

describe("execution-local immutable input files", () => {
  it("counts only complete successful reads, independently for each execution", () => {
    const files = new TaskInputFiles([input("Complete input")]);
    expect(files.unreadPaths()).toEqual([path]);
    files.read(path, 3);
    expect(files.unreadPaths()).toEqual([path]);
    expect(() => files.read(path, 0)).toThrow();
    expect(files.unreadPaths()).toEqual([path]);
    files.read(path, 65536);
    expect(files.unreadPaths()).toEqual([]);
    expect(new TaskInputFiles([input("Complete input")]).unreadPaths()).toEqual([path]);
  });

  it("reads exact supplied bytes and bound metadata without writing them to disk", async () => {
    const original = input();
    const files = new TaskInputFiles([original]);
    expect(await readTaskFile(path, 65536, files)).toEqual({ path, content: original.content,
      bytes: Buffer.byteLength(original.content), returned_bytes: Buffer.byteLength(original.content), truncated: false,
      input: { name: original.name, source, sha256: original.sha256, readOnly: true } });
    expect(files.instructions()).toContain(path);
    expect(files.instructions()).toContain(original.sha256);
    expect(files.instructions()).not.toContain(original.content);
  });

  it("reads all bounded inputs with full provenance atomically in one virtual bundle", async () => {
    const original = Array.from({ length: 16 }, (_, i) => input(`Complete ${i}\r\n✓ 日本語 🌍`, `file-${i}.md`));
    const expected = structuredClone(original).sort((a, b) => a.name.localeCompare(b.name));
    const files = new TaskInputFiles(original);
    original[0].content = "Changed after admission";
    original[0].source.runNonce = "changed";
    const partial = await readTaskFile(TASK_INPUT_ROOT, 100, files);
    expect(partial.truncated).toBe(true);
    expect(files.unreadPaths()).toHaveLength(16);
    const complete = await readTaskFile(TASK_INPUT_ROOT, 65536, files);
    expect(complete.truncated).toBe(false);
    expect(complete.returned_bytes).toBe(Buffer.byteLength(complete.content));
    expect(complete.bytes).toBeLessThanOrEqual(65536);
    expect(JSON.parse(complete.content)).toEqual(expected);
    expect(files.unreadPaths()).toEqual([]);
    await expect(writeTaskFile(TASK_INPUT_ROOT, "overwrite")).rejects.toThrow("read-only");
    await expect(readTaskFile(TASK_INPUT_ROOT, 65536)).rejects.toThrow("not supplied");
  });

  it("freezes a private snapshot and cannot leak across executions or reads", async () => {
    const original = input("First"); const caller = [original]; const first = new TaskInputFiles(caller);
    original.content = "Mutation"; original.source.runNonce = "changed"; caller.length = 0;
    const second = new TaskInputFiles([input("Second")]);
    const [one, two] = await Promise.all([readTaskFile(path, 100, first), readTaskFile(path, 100, second)]);
    expect(one.content).toBe("First"); expect(two.content).toBe("Second");
    expect(one.input?.source.runNonce).toBe("run-first");
    expect(() => { one.input!.source.runNonce = "mutated-result"; }).toThrow();
    one.content = "mutated-result";
    expect((await readTaskFile(path, 100, first)).content).toBe("First");
    await expect(readTaskFile(path, 100)).rejects.toThrow("not supplied");
    await expect(readTaskFile(`${TASK_INPUT_ROOT}/other.md`, 100, first)).rejects.toThrow("not supplied");
  });

  it.each([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 100])("truncates only at complete UTF-8 characters at bound %s", async limit => {
    const content = "abc✓🌍tail";
    const result = await readTaskFile(path, limit, new TaskInputFiles([input(content)]));
    expect(result.content).not.toContain("�");
    expect(Buffer.byteLength(result.content)).toBe(result.returned_bytes);
    expect(result.returned_bytes).toBeLessThanOrEqual(limit);
    expect(result.bytes).toBe(Buffer.byteLength(content));
    expect(result.truncated).toBe(result.returned_bytes < result.bytes);
    expect(content.startsWith(result.content)).toBe(true);
    expect(result.input?.sha256).toBe(missionInputContentDigest(content));
  });

  it("rejects bounds too small for the first character rather than counting an empty read", async () => {
    await expect(readTaskFile(path, 1, new TaskInputFiles([input("✓tail")]))).rejects.toThrow("first UTF-8 character");
    expect((await readTaskFile(path, 3, new TaskInputFiles([input("✓tail")]))).content).toBe("✓");
  });

  it.each([0, -1, NaN, Infinity, 0.5, 16 * 1024 * 1024 + 1])("rejects invalid read bound %s", async limit => {
    await expect(readTaskFile(path, limit, new TaskInputFiles([input()]))).rejects.toThrow("bound");
  });

  it.each([`${TASK_INPUT_ROOT}/`, `${TASK_INPUT_ROOT}/../escape`, `${TASK_INPUT_ROOT}/draft.md/..`,
    `${TASK_INPUT_ROOT}//draft.md`, "/sandbox//.kars-inputs/draft.md", "/sandbox/./.kars-inputs/draft.md",
    "/sandbox/.kars-inputs/./draft.md", "/tmp/../sandbox/.kars-inputs/draft.md", "/tmp/.kars-inputs/draft.md",
    "/sandbox/.kars-inputs/../../tmp/out", "/sandbox//.kars-inputs/../out", ".kars-inputs/draft.md"])("denies reserved path alias or traversal %s without disk fallback", async alias => {
    await expect(readTaskFile(alias, 100, new TaskInputFiles([input()]))).rejects.toThrow();
    await expect(readTaskFile(alias, 100)).rejects.toThrow("not supplied");
    await expect(writeTaskFile(alias, "overwrite")).rejects.toThrow("read-only");
  });

  it("denies exact reserved writes even without an input list and never falls through to a real file", async () => {
    await expect(writeTaskFile(path, "overwrite")).rejects.toThrow("read-only");
    const directory = await mkdtemp("/tmp/kars-input-isolation-");
    try {
      const ordinary = `${directory}/ordinary.md`;
      const blocked = `${directory}/.kars-inputs`;
      await writeFile(ordinary, "abc✓tail"); await writeFile(blocked, "must not expose disk data");
      await expect(readTaskFile(blocked, 100, new TaskInputFiles([input()]))).rejects.toThrow();
      expect(await readTaskFile(ordinary, 5, new TaskInputFiles([input()]))).toEqual({ path: ordinary, bytes: 10, returned_bytes: 5, truncated: true, content: "abc�" });
    } finally { await rm(directory, { recursive: true, force: true }); }
  });
});
