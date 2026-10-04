// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile, access } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { afterEach, describe, expect, it, vi } from "vitest";
import { readTaskFile, writeTaskFile } from "./task-filesystem.js";

const execute = promisify(execFile);
const directories: string[] = [];
async function workspace(): Promise<string> {
  const path = await mkdtemp("/tmp/kars-filesystem-wrapper-");
  directories.push(path);
  return path;
}
afterEach(async () => {
  vi.unstubAllEnvs();
  await Promise.all(directories.splice(0).map(path => rm(path, { recursive: true, force: true })));
});

describe("packaged filesystem helper", () => {
  it("passes descriptor-relative syscall, race and partial-I/O regressions", async () => {
    const test = fileURLToPath(new URL("./task-filesystem.test.py", import.meta.url));
    const result = await execute("/usr/bin/python3", ["-I", "-S", "-B", test], { timeout: 10_000, env: {} });
    expect(result.stderr).toMatch(/Ran \d+ tests/);
    expect(result.stderr).toContain("OK");
  });

  it("preserves Node UTF-8 encoding, CRLF and replacement-character semantics", async () => {
    const path = join(await workspace(), "nested", "document.md");
    const content = "Exact bytes\r\n✓\ud800";
    const encoded = Buffer.from(content, "utf8");
    expect(await writeTaskFile(path, content)).toBe(encoded.length);
    expect(await readFile(path)).toEqual(encoded);
    expect(await readTaskFile(path, 1024)).toEqual({
      path, bytes: encoded.length, returned_bytes: encoded.length,
      truncated: false, content: encoded.toString("utf8"),
    });
  });

  it("supports exactly 16 MiB across the bounded process interface", async () => {
    const path = join(await workspace(), "large.txt");
    const content = "x".repeat(16 * 1024 * 1024);
    expect(await writeTaskFile(path, content)).toBe(content.length);
    const result = await readTaskFile(path, content.length);
    expect(result.bytes).toBe(content.length);
    expect(result.returned_bytes).toBe(content.length);
    expect(result.truncated).toBe(false);
    expect(result.content === content).toBe(true);
  });

  it("rejects oversized UTF-8 writes before creating a directory", async () => {
    const path = join(await workspace(), "new", "large.txt");
    await expect(writeTaskFile(path, "✓".repeat(6 * 1024 * 1024))).rejects.toThrow("16777216 bytes");
    await expect(access(join(path, ".."))).rejects.toThrow();
  });

  it("reports an empty file and actual capped bytes without padding", async () => {
    const path = join(await workspace(), "document.txt");
    await writeTaskFile(path, "");
    expect(await readTaskFile(path, 1)).toEqual({ path, bytes: 0, returned_bytes: 0, truncated: false, content: "" });
    await writeTaskFile(path, "abc✓tail");
    expect(await readTaskFile(path, 5)).toEqual({ path, bytes: 10, returned_bytes: 5, truncated: true, content: "abc�" });
  });

  it("fails closed on helper errors and invalid read bounds", async () => {
    const path = join(await workspace(), "missing.txt");
    await expect(readTaskFile(path, 10)).rejects.toThrow();
    for (const limit of [0, -1, NaN, Infinity, 0.5, 16 * 1024 * 1024 + 1]) {
      await expect(readTaskFile(path, limit)).rejects.toThrow();
    }
    await expect(writeTaskFile("relative/document.txt", "content")).rejects.toThrow();
    await expect(writeTaskFile("/tmp/../etc/document.txt", "content")).rejects.toThrow();
  });

  it("ignores inherited Python module injection", async () => {
    const directory = await workspace();
    const sentinel = join(directory, "injected");
    await writeFile(join(directory, "base64.py"), `open(${JSON.stringify(sentinel)}, "w").write("injected")\nraise RuntimeError("injected")\n`);
    vi.stubEnv("PYTHONPATH", directory);
    vi.stubEnv("PYTHONHOME", directory);
    const path = join(directory, "document.txt");
    expect(await writeTaskFile(path, "safe")).toBe(4);
    expect((await readTaskFile(path, 4)).content).toBe("safe");
    await expect(access(sentinel)).rejects.toThrow();
  });

  it.each(["{", "null", '{"operation":"unknown"}', '{"operation":"write","path":"/tmp/x","base64":"!"}'])(
    "rejects malformed process requests with bounded structured errors: %s", async input => {
      const helper = fileURLToPath(new URL("./task-filesystem.py", import.meta.url));
      const result = await new Promise<{ code: number | string | null | undefined; stdout: string }>(resolve => {
        const child = execFile("/usr/bin/python3", ["-I", "-S", "-B", helper],
          { timeout: 10_000, env: {}, encoding: "utf8" },
          (error, stdout) => resolve({ code: error?.code, stdout }));
        child.stdin!.end(input);
      });
      expect(result.code).toBe(1);
      const parsed = JSON.parse(result.stdout);
      expect(Object.keys(parsed)).toEqual(["error"]);
      expect(typeof parsed.error).toBe("string");
      expect(result.stdout.length).toBeLessThan(2048);
    },
  );
});
