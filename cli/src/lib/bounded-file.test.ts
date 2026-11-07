// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import { randomUUID } from "node:crypto";
import { chmodSync, linkSync, mkdirSync, renameSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import path from "node:path";
import { readBoundedFileSync } from "./bounded-file.js";

const hooks = vi.hoisted(() => ({ opened: undefined as (() => void) | undefined, reading: undefined as (() => void) | undefined }));
vi.mock("node:fs", async original => {
  const fs = await original<typeof import("node:fs")>();
  return { ...fs,
    openSync: (...args: Parameters<typeof fs.openSync>) => {
      const fd = fs.openSync(...args);
      hooks.opened?.();
      return fd;
    },
    readSync: (...args: Parameters<typeof fs.readSync>) => {
      const hook = hooks.reading; hooks.reading = undefined; hook?.();
      return fs.readSync(...args);
    },
  };
});
const directories: string[] = [];
function fixture(content = "review") {
  const directory = path.resolve(`.bounded-file-test-${randomUUID()}`);
  mkdirSync(directory, { mode: 0o700 }); directories.push(directory);
  const file = path.join(directory, "review");
  writeFileSync(file, content, { mode: 0o600 });
  return { directory, file };
}
afterEach(() => {
  hooks.opened = undefined; hooks.reading = undefined;
  for (const directory of directories.splice(0)) rmSync(directory, { recursive: true, force: true });
});
const read = (file: string, limit = 6, privateFile = false) => readBoundedFileSync(file, limit, "Unsafe review", privateFile);

describe("descriptor-bound review input", () => {
  it("reads empty, UTF-8 and exactly bounded regular files", () => {
    for (const content of ["", "review", "ééé"]) {
      expect(read(fixture(content).file, 6, true)).toBe(content);
    }
  });
  it("rejects oversized files and directories", () => {
    const { file, directory } = fixture("oversize");
    expect(() => read(file)).toThrow("Unsafe review");
    expect(() => read(directory)).toThrow("Unsafe review");
  });
  it.each(["existing", "dangling"])("rejects %s symlinks for private state without following them", kind => {
    const { file, directory } = fixture();
    const link = path.join(directory, "link");
    symlinkSync(kind === "existing" ? file : path.join(directory, "absent"), link);
    expect(() => read(link, 6, true)).toThrow("Unsafe review");
  });
  it("preserves symlink support for public reviewed inputs while bounding the target", () => {
    const { file, directory } = fixture();
    const link = path.join(directory, "link");
    symlinkSync(file, link);
    expect(read(link)).toBe("review");
    writeFileSync(file, "oversized");
    expect(() => read(link)).toThrow("Unsafe review");
  });
  it.runIf(typeof process.getuid === "function")("rejects private state owned by another user", () => {
    const { file } = fixture();
    const getuid = vi.spyOn(process, "getuid").mockReturnValue(process.getuid!() + 1);
    try { expect(() => read(file, 6, true)).toThrow("Unsafe review"); }
    finally { getuid.mockRestore(); }
  });
  it("preserves missing-file error codes for optional recovery records", () => {
    const { directory } = fixture();
    expect(() => read(path.join(directory, "absent"))).toThrow(expect.objectContaining({ code: "ENOENT" }));
  });
  it("rejects public permissions and hardlinks for private state only", () => {
    const { file, directory } = fixture();
    chmodSync(file, 0o644);
    expect(() => read(file, 6, true)).toThrow("Unsafe review");
    expect(read(file)).toBe("review");
    chmodSync(file, 0o600); linkSync(file, path.join(directory, "hardlink"));
    expect(() => read(file, 6, true)).toThrow("Unsafe review");
  });
  it("never reopens a path replaced by a symlink after opening", () => {
    const { file, directory } = fixture();
    const other = path.join(directory, "other"); writeFileSync(other, "unsafe", { mode: 0o600 });
    hooks.opened = () => {
      hooks.opened = undefined;
      renameSync(file, path.join(directory, "original")); symlinkSync(other, file);
    };
    expect(read(file, 6, true)).toBe("review");
  });
  it.each(["growth", "permissions", "same-size-write"])("rejects %s during the bounded read", change => {
    const { file } = fixture();
    hooks.reading = () => {
      if (change === "permissions") chmodSync(file, 0o644);
      else writeFileSync(file, change === "growth" ? "too large" : "edited");
    };
    expect(() => read(file, 6, true)).toThrow("Unsafe review");
  });
});
