// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import {
  inclusionEntryHash, parseInclusionSnapshot, readInclusionChain, type InclusionEntry,
} from "./receipt-log.js";

const run = vi.hoisted(() => vi.fn());
vi.mock("execa", () => ({ execa: run }));
afterEach(() => vi.resetAllMocks());

function chain(count = 3): InclusionEntry[] {
  const entries: InclusionEntry[] = [];
  for (let seq = 0; seq < count; seq++) {
    const receipt = `ns/r${seq}`;
    const prevHash = entries.at(-1)?.entryHash ?? "genesis";
    entries.push({
      seq, receipt, prevHash, payloadSha256: "digest",
      entryHash: inclusionEntryHash(seq, receipt, "digest", prevHash),
    });
  }
  return entries;
}

function map(index: number, entries: InclusionEntry[]) {
  return {
    metadata: {
      name: index === 0 ? "kars-receipt-log" : `kars-receipt-log-${String(index).padStart(6, "0")}`,
      namespace: "custom-kars", uid: `uid-${index}`, resourceVersion: "4",
      labels: index === 0 ? {} : { "app.kubernetes.io/component": "receipt-inclusion-log-segment" },
    },
    data: {
      "chain.json": JSON.stringify(entries),
      ...(index > 0 ? { segmentIndex: String(index), previousRootHash: entries[0]?.prevHash } : {}),
    },
  };
}

function snapshot(items: unknown[]) {
  return { metadata: { resourceVersion: "5" }, items };
}

describe("receipt log snapshot", () => {
  it("retains legacy heads without labels, empty logs and unrelated maps", () => {
    expect(parseInclusionSnapshot(snapshot([]), "custom-kars")).toBeNull();
    expect(parseInclusionSnapshot(snapshot([map(0, [])]), "custom-kars")).toEqual([]);
    expect(parseInclusionSnapshot(snapshot([
      { metadata: { name: "unrelated" } }, map(0, chain()),
    ]), "custom-kars")).toEqual(chain());
  });

  it("combines out-of-order segments with continuous sequence and exact roots", () => {
    const entries = chain();
    expect(parseInclusionSnapshot(snapshot([
      map(2, entries.slice(2)), map(0, entries.slice(0, 1)), map(1, entries.slice(1, 2)),
    ]), "custom-kars")).toEqual(entries);
  });

  it.each([
    ["missing head", [map(1, chain().slice(1))]],
    ["gap", [map(0, chain().slice(0, 1)), map(2, chain().slice(1))]],
    ["duplicate", [map(0, chain()), map(0, chain())]],
    ["empty overflow", [map(0, chain()), map(1, [])]],
  ])("rejects %s", (_, maps) => {
    expect(() => parseInclusionSnapshot(snapshot(maps), "custom-kars")).toThrow();
  });

  it.each([
    ["invalid JSON", "{private-payload"],
    ["object", "{}"],
    ["null entry", "[null]"],
    ["bad shape", "[{}]"],
    ["unsafe seq", '[{"seq":9007199254740992,"receipt":"r","payloadSha256":"d","prevHash":"genesis","entryHash":"h"}]'],
  ])("fails closed on %s without echoing stored data", (_, raw) => {
    const head = map(0, chain());
    head.data["chain.json"] = raw;
    expect(() => parseInclusionSnapshot(snapshot([head]), "custom-kars")).toThrow(/Receipt/);
    try { parseInclusionSnapshot(snapshot([head]), "custom-kars"); } catch (error) {
      expect(String(error)).not.toContain("private-payload");
    }
  });

  it.each([
    ["UID", { uid: "" }],
    ["RV", { resourceVersion: "" }],
    ["namespace", { namespace: "wrong" }],
    ["deleting", { deletionTimestamp: "2026-09-10T00:00:00Z" }],
    ["foreign owner", { ownerReferences: [{ uid: "foreign" }] }],
    ["foreign label", { labels: { "app.kubernetes.io/component": "foreign" } }],
  ])("rejects invalid %s", (_, metadata) => {
    const head = map(0, chain());
    expect(() => parseInclusionSnapshot(snapshot([
      { ...head, metadata: { ...head.metadata, ...metadata } },
    ]), "custom-kars")).toThrow();
  });

  it.each([
    ["index", "segmentIndex", "2"],
    ["root", "previousRootHash", "genesis"],
  ])("rejects mismatched %s metadata", (_, key, value) => {
    const entries = chain();
    const overflow = map(1, entries.slice(1));
    expect(() => parseInclusionSnapshot(snapshot([
      map(0, entries.slice(0, 1)), { ...overflow, data: { ...overflow.data, [key]: value } },
    ]), "custom-kars")).toThrow(/binding/);
  });

  it("rejects a valid-shaped altered chain", () => {
    const entries = chain();
    entries[1].receipt = "other/receipt";
    expect(() => parseInclusionSnapshot(snapshot([map(0, entries)]), "custom-kars"))
      .toThrow("broken at seq 1");
  });

  it.each([
    {}, { items: [] }, { metadata: { resourceVersion: "" }, items: [] },
    { metadata: { resourceVersion: "5", continue: "next" }, items: [] },
  ])("rejects an incomplete snapshot %#", (value) => {
    expect(() => parseInclusionSnapshot(value, "custom-kars")).toThrow("incomplete");
  });

  it("uses one complete API snapshot in the explicitly configured namespace", async () => {
    run.mockResolvedValue({ failed: false, stdout: JSON.stringify(snapshot([map(0, chain())])) });
    expect(await readInclusionChain("custom-kars")).toEqual(chain());
    expect(run).toHaveBeenCalledExactlyOnceWith("kubectl", [
      "get", "configmaps", "-n", "custom-kars", "--chunk-size=0", "-o", "json",
    ], { stdio: "pipe", reject: false });
  });

  it("does not interpret failed API access or malformed output as absence", async () => {
    run.mockResolvedValue({ failed: true, stdout: "", stderr: "secret-value" });
    await expect(readInclusionChain("custom-kars")).rejects.toThrow("verify API access");
    run.mockResolvedValue({ failed: false, stdout: "secret-value" });
    await expect(readInclusionChain("custom-kars")).rejects.toThrow("not valid JSON");
  });
});
