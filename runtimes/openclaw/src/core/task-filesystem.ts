// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFile } from "node:child_process";
import { fileURLToPath } from "node:url";

const helper = fileURLToPath(new URL("./task-filesystem.py", import.meta.url));
const maxBytes = 16 * 1024 * 1024;

function operation(request: Record<string, unknown>): Promise<Record<string, unknown>> {
  return new Promise((resolve, reject) => {
    const child = execFile("/usr/bin/python3", ["-I", "-S", "-B", helper], {
      timeout: 10_000, maxBuffer: 24 * 1024 * 1024, encoding: "utf8",
      // No shell, Python module search overrides, or inherited credentials.
      env: {},
    }, (error, stdout) => {
      try {
        const response = JSON.parse(stdout);
        if (error || response.error || !response.result) {
          reject(new Error(typeof response.error === "string" ? response.error : "Filesystem helper failed or timed out"));
        } else {
          resolve(response.result);
        }
      } catch {
        reject(new Error("Filesystem helper unavailable or returned invalid evidence"));
      }
    });
    child.stdin!.on("error", () => { /* Process completion reports a failed pipe. */ });
    child.stdin!.end(JSON.stringify(request));
  });
}

export async function writeTaskFile(path: string, content: string): Promise<number> {
  const bytes = Buffer.from(content, "utf8");
  if (bytes.length > maxBytes) throw new Error("content exceeds 16777216 bytes");
  const result = await operation({ operation: "write", path, base64: bytes.toString("base64") });
  if (result.bytes !== bytes.length) throw new Error("Filesystem write byte count mismatch");
  return bytes.length;
}

export async function readTaskFile(path: string, limit: number): Promise<{
  path: string; bytes: number; truncated: boolean; returned_bytes: number; content: string;
}> {
  const result = await operation({ operation: "read", path, max_bytes: limit });
  if (typeof result.base64 !== "string" || typeof result.path !== "string"
    || typeof result.bytes !== "number" || !Number.isSafeInteger(result.bytes) || result.bytes < 0
    || typeof result.returned_bytes !== "number" || !Number.isSafeInteger(result.returned_bytes)
    || result.returned_bytes < 0 || result.returned_bytes > limit || result.returned_bytes > result.bytes
    || result.truncated !== (result.returned_bytes < result.bytes)) {
    throw new Error("Invalid filesystem read evidence");
  }
  const bytes = Buffer.from(result.base64, "base64");
  if (bytes.length !== result.returned_bytes) throw new Error("Filesystem read byte count mismatch");
  return { path: result.path, bytes: result.bytes, truncated: result.truncated as boolean,
    returned_bytes: bytes.length, content: bytes.toString("utf8") };
}
