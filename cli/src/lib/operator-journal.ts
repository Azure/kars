// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { constants, closeSync, fstatSync, fsyncSync, openSync, readFileSync, statSync, unlinkSync, writeSync } from "node:fs";
import { dirname } from "node:path";
import { canonical, digest, record } from "./private-activation.js";

const LIMIT = 64 * 1024 * 1024;

/** An append-only, fsynced local operator receipt. A stale lock is never automatically broken. */
export interface OperatorJournal<Receipt> { save(receipt: Receipt): Promise<void> }

export async function withOperatorJournal<Receipt, T>(
  file: string, create: boolean,
  action: (journal: OperatorJournal<Receipt>, current?: Receipt) => Promise<T>,
): Promise<T> {
  const lock = `${file}.lock`;
  const lockFd = openSync(lock, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600);
  let fd: number | undefined;
  try {
    writeSync(lockFd, `${process.pid}\n`);
    fsyncSync(lockFd);
    fd = openSync(file, constants.O_RDWR | constants.O_APPEND | constants.O_NOFOLLOW
      | (create ? constants.O_CREAT | constants.O_EXCL : 0), 0o600);
    const info = fstatSync(fd);
    if (!info.isFile() || info.nlink !== 1 || (info.mode & 0o077) !== 0
      || (process.getuid && info.uid !== process.getuid()) || info.size > LIMIT) {
      throw new Error("Migration receipt must be a private, bounded, operator-owned regular file");
    }
    if (create) {
      const directory = openSync(dirname(file), constants.O_RDONLY);
      try { fsyncSync(directory); } finally { closeSync(directory); }
    }
    let current: Receipt | undefined, previous = "";
    const content = readFileSync(fd, "utf8");
    if (content && !content.endsWith("\n")) throw new Error("Migration receipt has a torn final record; preserve it for explicit operator recovery");
    for (const line of content.split("\n").filter(Boolean)) {
      const envelope = record(JSON.parse(line));
      if (envelope.previous !== previous || envelope.digest !== digest(envelope.receipt)
        || Object.keys(envelope).sort().join(",") !== "digest,previous,receipt") throw new Error("Migration receipt integrity check failed");
      current = envelope.receipt as unknown as Receipt;
      previous = digest(envelope);
    }
    if (!create && !current) throw new Error("Migration receipt has no durable preparation; no resume is authorized");
    const descriptor = fd;
    const journal: OperatorJournal<Receipt> = { save: async receipt => {
      const envelope = { previous, digest: digest(receipt), receipt };
      const data = Buffer.from(`${canonical(envelope)}\n`);
      const before = fstatSync(descriptor), named = statSync(file);
      if (before.ino !== named.ino || before.dev !== named.dev || before.size + data.length > LIMIT) throw new Error("Migration receipt path or size changed");
      let offset = 0;
      while (offset < data.length) {
        const written = writeSync(descriptor, data, offset, data.length - offset);
        if (!written) throw new Error("Migration receipt write made no progress");
        offset += written;
      }
      fsyncSync(descriptor);
      current = structuredClone(receipt);
      previous = digest(envelope);
    } };
    return await action(journal, current);
  } finally {
    if (fd !== undefined) closeSync(fd);
    closeSync(lockFd);
    unlinkSync(lock);
  }
}
