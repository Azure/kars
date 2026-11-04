import { beforeEach, describe, expect, it, vi } from "vitest";

const helper = vi.hoisted(() => ({ response: {} as Record<string, unknown> }));
vi.mock("node:child_process", () => ({
  execFile: vi.fn((_file, _args, _options, callback) => ({ stdin: {
    on: vi.fn(),
    end: () => callback(null, JSON.stringify({ result: helper.response })),
  } })),
}));

import { readTaskFile } from "./task-filesystem.js";

function readResult(bytes: Buffer) {
  helper.response = { path: "/sandbox/report.md", base64: bytes.toString("base64"),
    bytes: bytes.length, returned_bytes: bytes.length, truncated: false };
}

describe("native artifact UTF-8 readback", () => {
  beforeEach(() => { helper.response = {}; });
  it("rejects invalid UTF-8 rather than delivering replacement text", async () => {
    readResult(Buffer.from([0x61, 0xff, 0x62]));
    await expect(readTaskFile("/sandbox/report.md", 128, undefined, undefined, true)).rejects.toThrow();
  });
  it("preserves the legacy permissive read behavior", async () => {
    readResult(Buffer.from([0x61, 0xff, 0x62]));
    expect((await readTaskFile("/sandbox/report.md", 128)).content).toBe("a\ufffdb");
  });
  it("preserves a real BOM and valid replacement characters byte-for-byte", async () => {
    const content = "\ufeffUnicode report: \ufffd café";
    readResult(Buffer.from(content));
    const result = await readTaskFile("/sandbox/report.md", 128, undefined, undefined, true);
    expect(result.content).toBe(content);
    expect(Buffer.byteLength(result.content)).toBe(result.returned_bytes);
  });
});
