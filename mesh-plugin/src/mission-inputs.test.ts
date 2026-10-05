// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { MAX_MISSION_INPUT_BYTES, missionInputContentDigest, missionInputReferences, missionInputs, missionInputsDigest } from "./mission-inputs.js";

const source = { namespace: "kars-system", taskName: "writer", taskUid: "task-uid", sandboxUid: "sandbox-uid",
  podUid: "pod-uid", runNonce: "run-first", assignmentId: "assignment-first", agentDid: `did:mesh:${"a".repeat(32)}`, artifactName: "draft.md" };
function input(name = "draft.md", content = "Original bytes\r\n✓") {
  return { name, content, sha256: missionInputContentDigest(content), source: { ...source } };
}

describe("immutable mission input contract", () => {
  it("snapshots canonical content-free references without accepting caller bytes", () => {
    const a = { name: "a.md", source: { ...source }, sha256: input().sha256 };
    const z = { ...a, name: "z.md", source: { ...source } };
    const result = missionInputReferences([z, a]);
    a.source.assignmentId = "changed";
    expect(result.map(item => item.name)).toEqual(["a.md", "z.md"]);
    expect(result[0].source.assignmentId).toBe("assignment-first");
    expect([result, result[0], result[0].source].every(Object.isFrozen)).toBe(true);
    expect(result[0]).not.toHaveProperty("content");
    expect(() => missionInputReferences([input()])).toThrow();
    expect(() => missionInputs(result)).toThrow();
  });

  it("rejects incomplete, extra, malformed and accessor reference pins", () => {
    const reference = { name: "draft.md", source: { ...source }, sha256: input().sha256 };
    const getter = vi.fn(() => "bytes");
    const accessor = { ...reference }; Object.defineProperty(accessor, "sha256", { get: getter, enumerable: true });
    for (const value of [[], [{ ...reference, sha256: "sha256:BAD" }], [{ ...reference, source: { ...source, podUid: "" } }],
      [{ ...reference, name: "../draft.md" }], [{ ...reference, extra: true }], [reference, reference], [accessor]]) {
      expect(() => missionInputReferences(value)).toThrow();
    }
    expect(getter).not.toHaveBeenCalled();
  });

  it("canonicalizes aliases and own-field ordering while preserving exact content", () => {
    const a = input("a.md"), z = input("z.md");
    const reversed = { source: { ...z.source }, content: z.content, sha256: z.sha256, name: z.name };
    expect(missionInputs([reversed, a]).map(item => item.name)).toEqual(["a.md", "z.md"]);
    expect(missionInputsDigest([reversed, a])).toBe(missionInputsDigest([a, z]));
    expect(missionInputs([a])[0].content).toBe("Original bytes\r\n✓");
    expect(missionInputsDigest([input("a.md", "Original bytes\n✓")])).not.toBe(missionInputsDigest([a]));
  });

  it("copies and freezes all caller-owned input records before returning", () => {
    const item = input();
    const items = [item];
    const captured = missionInputs(items);
    item.content = "changed"; item.source.runNonce = "run-second"; items.splice(0);
    expect(captured[0].content).toBe("Original bytes\r\n✓");
    expect(captured[0].source.runNonce).toBe("run-first");
    expect([captured, captured[0], captured[0].source].every(Object.isFrozen)).toBe(true);
  });

  it.each(["", "../draft.md", "draft/name", "constructor", "prototype", "__proto__", "x".repeat(129)])("rejects unsafe alias %j", name => {
    expect(() => missionInputs([input(name)])).toThrow();
  });

  it("binds every producer/run field and the content digest", () => {
    const original = input();
    const digest = missionInputsDigest([original]);
    for (const key of ["namespace", "taskName", "taskUid", "sandboxUid", "podUid", "runNonce", "assignmentId", "artifactName"] as const) {
      const changed = input(); changed.source[key] = "different";
      expect(missionInputsDigest([changed])).not.toBe(digest);
    }
    const changed = input(); changed.source.agentDid = `did:mesh:${"b".repeat(32)}`;
    expect(missionInputsDigest([changed])).not.toBe(digest);
    expect(() => missionInputs([{ ...original, content: "tampered" }])).toThrow("digest");
    expect(() => missionInputs([{ ...original, sha256: "sha256:wrong" }])).toThrow("digest");
  });

  it.each(["\ud800", "\udfff", "  \r\n", ""])('rejects nonexact or blank UTF-8 %j', content => {
    expect(() => missionInputs([input("draft.md", content)])).toThrow();
  });

  it("enforces serialized UTF-8 size, unique names and collection bounds", () => {
    expect(() => missionInputs([])).toThrow();
    expect(() => missionInputs([input(), input()])).toThrow();
    expect(missionInputs(Array.from({ length: 16 }, (_, i) => input(`file-${i}.md`)))).toHaveLength(16);
    expect(() => missionInputs(Array.from({ length: 17 }, (_, i) => input(`file-${i}.md`)))).toThrow();
    for (const content of ["x".repeat(MAX_MISSION_INPUT_BYTES), "✓".repeat(MAX_MISSION_INPUT_BYTES / 3), "\n".repeat(40_000) + "x"]) {
      expect(() => missionInputs([input("draft.md", content)])).toThrow();
    }
    const small = input("draft.md", "x");
    const overhead = Buffer.byteLength(JSON.stringify([small])) - 1;
    expect(missionInputs([input("draft.md", "x".repeat(MAX_MISSION_INPUT_BYTES - overhead))])).toHaveLength(1);
    expect(() => missionInputs([input("draft.md", "x".repeat(MAX_MISSION_INPUT_BYTES - overhead + 1))])).toThrow();
  });

  it("rejects accessors, hidden/unknown fields, symbols, sparse and exotic containers without invoking getters", () => {
    const getter = vi.fn(() => "secret");
    const item = input(); Object.defineProperty(item, "content", { get: getter, enumerable: true });
    const list = [input()]; Object.defineProperty(list, "0", { get: getter, enumerable: true });
    const hidden = input(); Object.defineProperty(hidden, "content", { value: hidden.content, enumerable: false });
    const nested = input(); Object.defineProperty(nested.source, "runNonce", { get: getter, enumerable: true });
    const sparse: unknown[] = []; sparse.length = 1;
    for (const value of [[item], list, [hidden], [nested], [Object.assign(input(), { extra: 1 })],
      [Object.assign(input(), { [Symbol("extra")]: 1 })], sparse, Object.assign([input()], { extra: 1 }),
      [Object.setPrototypeOf(input(), { custom: true })]]) {
      expect(() => missionInputs(value)).toThrow();
    }
    expect(getter).not.toHaveBeenCalled();
  });

  it("rejects invalid source identities and accepts null-prototype records", () => {
    for (const patch of [{ namespace: "Invalid Namespace" }, { runNonce: "run\n" }, { agentDid: "did:other:123" }, { artifactName: "../secret" }]) {
      expect(() => missionInputs([{ ...input(), source: { ...source, ...patch } }])).toThrow();
    }
    const item = Object.assign(Object.create(null), input());
    item.source = Object.assign(Object.create(null), source);
    expect(missionInputs([item])[0].source).toEqual(source);
  });
});
