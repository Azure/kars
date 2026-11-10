// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { closeSync, constants, fstatSync, openSync, readSync } from "node:fs";

/** Read one validated descriptor with a growth bound; private state also forbids symlinks and shared access. */
export function readBoundedFileSync(file: string, limit: number, message: string, privateFile = false): string {
  let fd: number;
  try {
    fd = openSync(file, constants.O_RDONLY | constants.O_NONBLOCK | (privateFile ? constants.O_NOFOLLOW : 0));
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ELOOP") throw new Error(message);
    throw error;
  }
  try {
    const before = fstatSync(fd);
    if (!before.isFile() || before.size > limit
      || (privateFile && (before.nlink !== 1 || (before.mode & 0o077) !== 0
        || (process.getuid && before.uid !== process.getuid())))) throw new Error(message);
    const bytes = Buffer.alloc(limit + 1);
    let length = 0;
    while (length < bytes.length) {
      const count = readSync(fd, bytes, length, bytes.length - length, null);
      if (!count) break;
      length += count;
    }
    const after = fstatSync(fd);
    if (length > limit || length !== after.size || before.size !== after.size
      || before.mtimeMs !== after.mtimeMs || before.ctimeMs !== after.ctimeMs
      || before.mode !== after.mode || before.uid !== after.uid || before.nlink !== after.nlink) throw new Error(message);
    return bytes.subarray(0, length).toString("utf8");
  } finally {
    closeSync(fd);
  }
}
